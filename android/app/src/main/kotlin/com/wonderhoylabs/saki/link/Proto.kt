package com.wonderhoylabs.saki.link

import java.io.EOFException
import java.io.IOException
import java.io.InputStream
import java.net.Inet4Address
import java.net.Inet6Address
import java.net.InetAddress
import java.nio.ByteBuffer

/**
 * The Saki wire protocol, matching `crates/saki-core/src/proto.rs`. ProtoTest checks the same
 * bytes as the Rust tests do, and `docs/protocol.md` describes it all for people.
 */
object Proto {
	const val SERVICE_UUID = "53414b49-a776-4be4-9cf4-8ac46746a50f"
	const val INFO_UUID = "53414b49-a777-4be4-9cf4-8ac46746a50f"
	const val VERSION = 1
	const val MTU = 1280

	val MAGIC = byteArrayOf('S'.code.toByte(), 'A'.code.toByte(), 'K'.code.toByte(), 'I'.code.toByte())

	val GATEWAY_V4 = ip("10.233.77.1")
	val CLIENT_V4 = ip("10.233.77.2")

	/** Where the client resolver's own queries come from. Nothing else sends from this address. */
	val RESOLVER_V4 = ip("10.233.77.3")
	const val PREFIX_V4 = 24
	val GATEWAY_V6 = ip("fd53:616b:6900::1")
	val CLIENT_V6 = ip("fd53:616b:6900::2")
	const val PREFIX_V6 = 64
	val DEFAULT_DNS = ip("1.1.1.1")

	const val BYE_NORMAL = 0
	const val BYE_VERSION_MISMATCH = 1

	internal const val TYPE_HELLO = 1
	internal const val TYPE_WELCOME = 2
	internal const val TYPE_PACKET = 3
	internal const val TYPE_PING = 4
	internal const val TYPE_PONG = 5
	internal const val TYPE_BYE = 6
	internal const val HEADER_LEN = 3
	internal const val WELCOME_LEN = 2 + 4 + 4 + 1 + 16 + 16 + 1 + 4

	/** Parses an address literal. Never does a DNS lookup, since it only gets literals. */
	private fun ip(literal: String): InetAddress = InetAddress.getByName(literal)

	fun encodeInfo(psm: Int): ByteArray = MAGIC + byteArrayOf(VERSION.toByte(), (psm shr 8).toByte(), psm.toByte())

	/** Returns the peer's protocol version and PSM. */
	fun decodeInfo(value: ByteArray): Pair<Int, Int> {
		if (value.size < 7 || !value.copyOf(4).contentEquals(MAGIC)) {
			throw IOException("not a Saki info characteristic")
		}
		val psm = ((value[5].toInt() and 0xff) shl 8) or (value[6].toInt() and 0xff)
		return value[4].toInt() and 0xff to psm
	}

	internal fun decodeWelcome(payload: ByteArray): Frame.Welcome {
		if (payload.size < WELCOME_LEN) throw IOException("welcome frame is too short")
		val buffer = ByteBuffer.wrap(payload)
		fun address(length: Int): InetAddress {
			val bytes = ByteArray(length)
			buffer.get(bytes)
			return InetAddress.getByAddress(bytes)
		}
		val mtu = buffer.short.toInt() and 0xffff
		return Frame.Welcome(
			mtu = mtu,
			clientV4 = address(4) as Inet4Address,
			gatewayV4 = address(4) as Inet4Address,
			prefixV4 = buffer.get().toInt() and 0xff,
			clientV6 = address(16) as Inet6Address,
			gatewayV6 = address(16) as Inet6Address,
			prefixV6 = buffer.get().toInt() and 0xff,
			upstreamDns = address(4) as Inet4Address,
		)
	}
}

sealed interface Frame {
	data class Hello(val version: Int) : Frame

	data class Welcome(
		val mtu: Int,
		val clientV4: Inet4Address,
		val gatewayV4: Inet4Address,
		val prefixV4: Int,
		val clientV6: Inet6Address,
		val gatewayV6: Inet6Address,
		val prefixV6: Int,
		val upstreamDns: Inet4Address,
	) : Frame {
		companion object {
			fun standard(upstreamDns: InetAddress = Proto.DEFAULT_DNS) = Welcome(
				mtu = Proto.MTU,
				clientV4 = Proto.CLIENT_V4 as Inet4Address,
				gatewayV4 = Proto.GATEWAY_V4 as Inet4Address,
				prefixV4 = Proto.PREFIX_V4,
				clientV6 = Proto.CLIENT_V6 as Inet6Address,
				gatewayV6 = Proto.GATEWAY_V6 as Inet6Address,
				prefixV6 = Proto.PREFIX_V6,
				upstreamDns = upstreamDns as Inet4Address,
			)
		}
	}

	class Packet(val bytes: ByteArray, val length: Int = bytes.size) : Frame

	data class Ping(val nonce: Long) : Frame

	data class Pong(val nonce: Long) : Frame

	data class Bye(val reason: Int) : Frame

	/** The frame as it goes on the wire. */
	fun encode(): ByteArray {
		val payloadLength = when (this) {
			is Hello -> Proto.MAGIC.size + 1
			is Welcome -> Proto.WELCOME_LEN
			is Packet -> length
			is Ping, is Pong -> 8
			is Bye -> 1
		}
		val buffer = ByteBuffer.allocate(Proto.HEADER_LEN + payloadLength)
		val type = when (this) {
			is Hello -> Proto.TYPE_HELLO
			is Welcome -> Proto.TYPE_WELCOME
			is Packet -> Proto.TYPE_PACKET
			is Ping -> Proto.TYPE_PING
			is Pong -> Proto.TYPE_PONG
			is Bye -> Proto.TYPE_BYE
		}
		buffer.put(type.toByte())
		buffer.putShort(payloadLength.toShort())
		when (this) {
			is Hello -> {
				buffer.put(Proto.MAGIC)
				buffer.put(version.toByte())
			}

			is Welcome -> {
				buffer.putShort(mtu.toShort())
				buffer.put(clientV4.address)
				buffer.put(gatewayV4.address)
				buffer.put(prefixV4.toByte())
				buffer.put(clientV6.address)
				buffer.put(gatewayV6.address)
				buffer.put(prefixV6.toByte())
				buffer.put(upstreamDns.address)
			}

			is Packet -> buffer.put(bytes, 0, length)

			is Ping -> buffer.putLong(nonce)

			is Pong -> buffer.putLong(nonce)

			is Bye -> buffer.put(reason.toByte())
		}
		return buffer.array()
	}

	companion object {
		/**
		 * Reads the next frame this version understands. Frames of an unknown type are skipped, so
		 * that a later protocol version can add some that older peers just ignore.
		 */
		fun read(input: InputStream): Frame {
			val header = ByteArray(Proto.HEADER_LEN)
			while (true) {
				readFully(input, header)
				val length = ((header[1].toInt() and 0xff) shl 8) or (header[2].toInt() and 0xff)
				val payload = ByteArray(length)
				readFully(input, payload)
				decode(header[0].toInt() and 0xff, payload)?.let { return it }
			}
		}

		internal fun decode(type: Int, payload: ByteArray): Frame? = when (type) {
			Proto.TYPE_HELLO -> {
				val magicLength = Proto.MAGIC.size
				if (payload.size < magicLength + 1 || !payload.copyOf(magicLength).contentEquals(Proto.MAGIC)) {
					throw IOException("not a Saki hello")
				}
				Hello(payload[magicLength].toInt() and 0xff)
			}

			Proto.TYPE_WELCOME -> Proto.decodeWelcome(payload)

			Proto.TYPE_PACKET -> Packet(payload)

			Proto.TYPE_PING, Proto.TYPE_PONG -> {
				if (payload.size < 8) throw IOException("ping frame is too short")
				val nonce = ByteBuffer.wrap(payload).long
				if (type == Proto.TYPE_PING) Ping(nonce) else Pong(nonce)
			}

			Proto.TYPE_BYE -> {
				if (payload.isEmpty()) throw IOException("bye frame is empty")
				Bye(payload[0].toInt() and 0xff)
			}

			else -> null
		}

		private fun readFully(input: InputStream, into: ByteArray) {
			var done = 0
			while (done < into.size) {
				val count = input.read(into, done, into.size - done)
				if (count < 0) throw EOFException("the link closed")
				done += count
			}
		}
	}
}
