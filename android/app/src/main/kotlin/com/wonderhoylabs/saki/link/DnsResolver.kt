package com.wonderhoylabs.saki.link

import java.net.InetAddress

/**
 * The resolver a client runs for itself, matching `crates/saki-core/src/dns.rs`.
 *
 * Android is told to use the gateway address for DNS, which has nothing behind it. Queries to it
 * are picked out of the packet stream here, answered from a small cache if possible, and otherwise
 * sent on through the tunnel to the upstream resolver, which is 1.1.1.1 unless the server says
 * otherwise. A TCP query, which follows a truncated reply, goes through the tunnel like any other
 * connection, and the server's NAT sends it to the same upstream resolver.
 */
class DnsResolver(gateway: InetAddress, source: InetAddress, upstream: InetAddress) {
	sealed interface Outgoing {
		/** It isn't for the resolver. Send it through the tunnel unchanged. */
		data object Pass : Outgoing

		/** Send this query through the tunnel instead. */
		class Query(val packet: ByteArray) : Outgoing

		/** Hand this reply straight back to the system. */
		class Reply(val packet: ByteArray) : Outgoing

		/** It was for the resolver but can't be answered. */
		data object Drop : Outgoing
	}

	sealed interface Incoming {
		/** It isn't for the resolver. Give it to the system unchanged. */
		data object Pass : Incoming

		/** Give the system this reply instead. */
		class Reply(val packet: ByteArray) : Incoming

		/** It was addressed to the resolver, but nothing is waiting for it. */
		data object Drop : Incoming
	}

	private class Pending(val client: ByteArray, val clientPort: Int, val key: String?, val sentNanos: Long)

	private class CacheEntry(val response: ByteArray, val expiresNanos: Long)

	private val gateway = gateway.address
	private val source = source.address

	@Volatile private var upstream = upstream.address
	private var nextPort = FIRST_PORT
	private val pending = HashMap<Int, Pending>()
	private val cache = HashMap<String, CacheEntry>()

	/** Follows the server, in case it changed its mind between connections. */
	@Synchronized
	fun setUpstream(address: InetAddress) {
		if (!address.address.contentEquals(upstream)) {
			upstream = address.address
			cache.clear()
		}
	}

	@Synchronized
	fun outgoing(packet: ByteArray, length: Int, nowNanos: Long = System.nanoTime()): Outgoing {
		val udp = Udp.parse(packet, length) ?: return Outgoing.Pass
		if (!udp.dst.contentEquals(gateway) || udp.dstPort != DNS_PORT) return Outgoing.Pass
		if (udp.payload.size < DNS_HEADER_LEN) return Outgoing.Drop

		val key = cacheKey(udp.payload)
		val cached = key?.let { cache[it] }
		if (cached != null) {
			if (cached.expiresNanos - nowNanos > 0) {
				val response = cached.response.copyOf()
				// Answer with this query's ID and spelling of the name, in case it mixes case.
				System.arraycopy(udp.payload, 0, response, 0, 2)
				questionEnd(udp.payload)?.let { end ->
					System.arraycopy(udp.payload, DNS_HEADER_LEN, response, DNS_HEADER_LEN, end - DNS_HEADER_LEN)
				}
				return Outgoing.Reply(buildUdp(gateway, DNS_PORT, udp.src, udp.srcPort, response))
			}
			cache.remove(key)
		}

		expirePending(nowNanos)
		if (pending.size >= MAX_PENDING) return Outgoing.Drop
		val port = allocatePort()
		pending[port] = Pending(udp.src, udp.srcPort, key, nowNanos)
		val query = udp.payload.copyOf()
		limitEdnsSize(query)
		return Outgoing.Query(buildUdp(source, port, upstream, DNS_PORT, query))
	}

	@Synchronized
	fun incoming(packet: ByteArray, length: Int, nowNanos: Long = System.nanoTime()): Incoming {
		val udp = Udp.parse(packet, length) ?: return Incoming.Pass
		if (!udp.dst.contentEquals(source)) return Incoming.Pass
		if (!udp.src.contentEquals(upstream) || udp.srcPort != DNS_PORT) return Incoming.Drop
		val waiting = pending.remove(udp.dstPort) ?: return Incoming.Drop
		val key = waiting.key
		val ttl = cacheableTtlSeconds(udp.payload)
		if (key != null && ttl != null) {
			remember(key, CacheEntry(udp.payload, nowNanos + ttl * NANOS_PER_SECOND), nowNanos)
		}
		return Incoming.Reply(buildUdp(gateway, DNS_PORT, waiting.client, waiting.clientPort, udp.payload))
	}

	private fun remember(key: String, entry: CacheEntry, nowNanos: Long) {
		if (cache.size >= MAX_CACHE_ENTRIES) {
			cache.values.removeAll { it.expiresNanos - nowNanos <= 0 }
			if (cache.size >= MAX_CACHE_ENTRIES) cache.clear()
		}
		cache[key] = entry
	}

	private fun expirePending(nowNanos: Long) {
		pending.values.removeAll { nowNanos - it.sentNanos >= PENDING_TIMEOUT_NANOS }
	}

	private fun allocatePort(): Int {
		while (true) {
			val port = nextPort
			nextPort = if (port == 0xffff) FIRST_PORT else port + 1
			if (port !in pending) return port
		}
	}

	private class Udp(
		val src: ByteArray,
		val dst: ByteArray,
		val srcPort: Int,
		val dstPort: Int,
		val payload: ByteArray,
	) {
		companion object {
			/** Returns the datagram in an unfragmented IPv4 UDP packet, and null for anything else. */
			fun parse(packet: ByteArray, length: Int): Udp? {
				if (length < IPV4_HEADER_LEN + UDP_HEADER_LEN) return null
				if (packet[0].toInt() ushr 4 and 0x0f != 4 || packet[9].toInt() != 17) return null
				val headerLen = (packet[0].toInt() and 0x0f) * 4
				if (headerLen < IPV4_HEADER_LEN || u16(packet, 6) and 0x3fff != 0) return null
				val totalLen = minOf(u16(packet, 2), length)
				if (totalLen < headerLen + UDP_HEADER_LEN) return null
				val udpLen = u16(packet, headerLen + 4)
				if (udpLen < UDP_HEADER_LEN || headerLen + udpLen > totalLen) return null
				return Udp(
					src = packet.copyOfRange(12, 16),
					dst = packet.copyOfRange(16, 20),
					srcPort = u16(packet, headerLen),
					dstPort = u16(packet, headerLen + 2),
					payload = packet.copyOfRange(headerLen + UDP_HEADER_LEN, headerLen + udpLen),
				)
			}
		}
	}

	companion object {
		private const val IPV4_HEADER_LEN = 20
		private const val UDP_HEADER_LEN = 8
		private const val DNS_HEADER_LEN = 12
		private const val DNS_PORT = 53
		private const val FIRST_PORT = 1024
		private const val MAX_CACHE_ENTRIES = 512
		private const val MAX_CACHE_TTL_SECONDS = 300L
		private const val MAX_PENDING = 512
		private const val PENDING_TIMEOUT_NANOS = 15_000_000_000L
		private const val NANOS_PER_SECOND = 1_000_000_000L
		private const val MAX_EDNS_UDP_SIZE = 1232
		private const val DNS_TYPE_OPT = 41

		private fun u16(bytes: ByteArray, at: Int): Int =
			((bytes[at].toInt() and 0xff) shl 8) or (bytes[at + 1].toInt() and 0xff)

		private fun putU16(bytes: ByteArray, at: Int, value: Int) {
			bytes[at] = (value shr 8).toByte()
			bytes[at + 1] = value.toByte()
		}

		private fun sum(start: Int, bytes: ByteArray, from: Int, until: Int): Int {
			var total = start
			var index = from
			while (index + 1 < until) {
				total += u16(bytes, index)
				index += 2
			}
			if (index < until) total += (bytes[index].toInt() and 0xff) shl 8
			return total
		}

		private fun finish(start: Int): Int {
			var total = start
			while (total ushr 16 != 0) total = (total and 0xffff) + (total ushr 16)
			return total.inv() and 0xffff
		}

		/** Builds an IPv4 UDP packet with both checksums filled in. */
		fun buildUdp(src: ByteArray, srcPort: Int, dst: ByteArray, dstPort: Int, payload: ByteArray): ByteArray {
			val udpLen = UDP_HEADER_LEN + payload.size
			val totalLen = IPV4_HEADER_LEN + udpLen
			val packet = ByteArray(totalLen)

			packet[0] = 0x45
			putU16(packet, 2, totalLen)
			packet[6] = 0x40 // don't fragment
			packet[8] = 64
			packet[9] = 17
			System.arraycopy(src, 0, packet, 12, 4)
			System.arraycopy(dst, 0, packet, 16, 4)
			putU16(packet, 10, finish(sum(0, packet, 0, IPV4_HEADER_LEN)))

			val udp = IPV4_HEADER_LEN
			putU16(packet, udp, srcPort)
			putU16(packet, udp + 2, dstPort)
			putU16(packet, udp + 4, udpLen)
			System.arraycopy(payload, 0, packet, udp + UDP_HEADER_LEN, payload.size)

			var pseudo = sum(0, src, 0, 4)
			pseudo = sum(pseudo, dst, 0, 4)
			pseudo += 17 + udpLen
			val checksum = finish(sum(pseudo, packet, udp, totalLen))
			putU16(packet, udp + 6, if (checksum == 0) 0xffff else checksum)
			return packet
		}

		/** Where the single question in a DNS message ends, or null if it doesn't have exactly one. */
		private fun questionEnd(message: ByteArray): Int? {
			if (message.size < DNS_HEADER_LEN || u16(message, 4) != 1) return null
			val nameEnd = skipName(message, DNS_HEADER_LEN) ?: return null
			val end = nameEnd + 4
			return if (end <= message.size) end else null
		}

		/**
		 * The cache key for a query: its question in lowercase plus everything after it, so that
		 * queries asking for different EDNS options don't share an answer.
		 */
		private fun cacheKey(query: ByteArray): String? {
			val end = questionEnd(query) ?: return null
			val key = StringBuilder((query.size - DNS_HEADER_LEN) * 2)
			for (index in DNS_HEADER_LEN until query.size) {
				var byte = query[index].toInt() and 0xff
				if (index < end && byte in 'A'.code..'Z'.code) byte += 'a'.code - 'A'.code
				key.append(byte.toChar())
			}
			return key.toString()
		}

		/**
		 * How long a response may be cached, in seconds, or null if it shouldn't be. Only successful
		 * answers are kept. TTLs aren't rewritten when serving from the cache, so the cap also limits
		 * how stale a record can get in the system's own cache.
		 */
		private fun cacheableTtlSeconds(response: ByteArray): Long? {
			if (response.size < DNS_HEADER_LEN) return null
			val isResponse = response[2].toInt() and 0x80 != 0
			val truncated = response[2].toInt() and 0x02 != 0
			val rcode = response[3].toInt() and 0x0f
			val answers = u16(response, 6)
			val authorities = u16(response, 8)
			if (!isResponse || truncated || rcode != 0 || answers == 0) return null

			var offset = questionEnd(response) ?: return null
			var ttl = MAX_CACHE_TTL_SECONDS
			repeat(answers + authorities) {
				offset = skipName(response, offset) ?: return null
				if (offset + 10 > response.size) return null
				val recordTtl = (u16(response, offset + 4).toLong() shl 16) or u16(response, offset + 6).toLong()
				ttl = minOf(ttl, recordTtl)
				offset += 10 + u16(response, offset + 8)
				if (offset > response.size) return null
			}
			return if (ttl > 0) ttl else null
		}

		/** Lowers the reply size a query advertises in its EDNS record, if it has one. */
		private fun limitEdnsSize(query: ByteArray) {
			var offset = questionEnd(query) ?: return
			repeat(u16(query, 6) + u16(query, 8) + u16(query, 10)) {
				val afterName = skipName(query, offset) ?: return
				if (afterName + 10 > query.size) return
				if (u16(query, afterName) == DNS_TYPE_OPT) {
					// In an OPT record, the class field holds the UDP size.
					if (u16(query, afterName + 2) > MAX_EDNS_UDP_SIZE) putU16(query, afterName + 2, MAX_EDNS_UDP_SIZE)
					return
				}
				offset = afterName + 10 + u16(query, afterName + 8)
			}
		}

		private fun skipName(message: ByteArray, start: Int): Int? {
			var offset = start
			while (true) {
				if (offset >= message.size) return null
				val len = message[offset].toInt() and 0xff
				when {
					len == 0 -> return offset + 1

					len <= 63 -> offset += 1 + len

					// A compression pointer ends the name.
					len >= 0xc0 -> return if (offset + 1 < message.size) offset + 2 else null

					else -> return null
				}
			}
		}
	}
}
