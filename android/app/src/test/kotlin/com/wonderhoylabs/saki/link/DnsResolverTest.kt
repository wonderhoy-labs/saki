package com.wonderhoylabs.saki.link

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import java.net.InetAddress

class DnsResolverTest {
	private val gateway = InetAddress.getByName("10.233.77.1")
	private val client = InetAddress.getByName("10.233.77.2")
	private val source = InetAddress.getByName("10.233.77.3")
	private val upstream = InetAddress.getByName("1.1.1.1")

	private fun resolver() = DnsResolver(gateway, source, upstream)

	private fun query(id: Int, name: String = "example"): ByteArray {
		val header = byteArrayOf((id shr 8).toByte(), id.toByte(), 1, 0, 0, 1, 0, 0, 0, 0, 0, 0)
		val question = byteArrayOf(name.length.toByte()) + name.toByteArray() +
			byteArrayOf(3, 'c'.code.toByte(), 'o'.code.toByte(), 'm'.code.toByte(), 0, 0, 1, 0, 1)
		return header + question
	}

	private fun answer(query: ByteArray, ttl: Int): ByteArray {
		val message = query.copyOf()
		message[2] = 0x81.toByte()
		message[3] = 0x80.toByte()
		message[7] = 1
		return message + byteArrayOf(0xc0.toByte(), 0x0c, 0, 1, 0, 1) +
			byteArrayOf((ttl ushr 24).toByte(), (ttl ushr 16).toByte(), (ttl ushr 8).toByte(), ttl.toByte()) +
			byteArrayOf(0, 4, 93, 184.toByte(), 216.toByte(), 34)
	}

	private fun udp(from: InetAddress, fromPort: Int, to: InetAddress, toPort: Int, payload: ByteArray) =
		DnsResolver.buildUdp(from.address, fromPort, to.address, toPort, payload)

	private fun fromOs(payload: ByteArray) = udp(client, 5000, gateway, 53, payload)

	private fun DnsResolver.out(packet: ByteArray, now: Long = 0) = outgoing(packet, packet.size, now)

	private fun DnsResolver.into(packet: ByteArray, now: Long = 0) = incoming(packet, packet.size, now)

	private fun sourcePort(packet: ByteArray) = ((packet[20].toInt() and 0xff) shl 8) or (packet[21].toInt() and 0xff)

	private fun payload(packet: ByteArray) = packet.copyOfRange(28, packet.size)

	@Test
	fun unrelatedPacketsPassThrough() {
		val resolver = resolver()
		val elsewhere = udp(client, 5000, InetAddress.getByName("8.8.8.8"), 53, query(1))
		assertSame(DnsResolver.Outgoing.Pass, resolver.out(elsewhere))
		assertSame(DnsResolver.Outgoing.Pass, resolver.out(udp(client, 5000, gateway, 54, query(1))))
		assertSame(DnsResolver.Outgoing.Pass, resolver.out(byteArrayOf(0x60, 0, 0, 0)))
		assertSame(DnsResolver.Incoming.Pass, resolver.into(elsewhere))
	}

	@Test
	fun aQueryIsForwardedAndItsReplyComesBackFromTheGateway() {
		val resolver = resolver()
		val question = query(0x1234)

		val forwarded = (resolver.out(fromOs(question)) as DnsResolver.Outgoing.Query).packet
		assertArrayEquals(source.address, forwarded.copyOfRange(12, 16))
		assertArrayEquals(upstream.address, forwarded.copyOfRange(16, 20))
		assertArrayEquals(question, payload(forwarded))

		val reply = udp(upstream, 53, source, sourcePort(forwarded), answer(question, 60))
		val delivered = (resolver.into(reply) as DnsResolver.Incoming.Reply).packet
		assertArrayEquals(gateway.address, delivered.copyOfRange(12, 16))
		assertArrayEquals(client.address, delivered.copyOfRange(16, 20))
		assertEquals(53, sourcePort(delivered))
		assertArrayEquals(answer(question, 60), payload(delivered))
	}

	@Test
	fun aCachedAnswerUsesTheNewIdAndSpellingAndThenExpires() {
		val resolver = resolver()
		val first = query(1)
		val forwarded = (resolver.out(fromOs(first)) as DnsResolver.Outgoing.Query).packet
		resolver.into(udp(upstream, 53, source, sourcePort(forwarded), answer(first, 60)))

		val second = query(2, "ExAmPlE")
		val reply = (resolver.out(fromOs(second)) as DnsResolver.Outgoing.Reply).packet
		val message = payload(reply)
		assertEquals(2, message[1].toInt())
		assertEquals("ExAmPlE", String(message, 13, 7))

		val later = 61_000_000_000L
		assertTrue(resolver.out(fromOs(second), later) is DnsResolver.Outgoing.Query)
	}

	@Test
	fun failuresAreNotCached() {
		val resolver = resolver()
		val question = query(1)
		val forwarded = (resolver.out(fromOs(question)) as DnsResolver.Outgoing.Query).packet
		val failure = answer(question, 60)
		failure[3] = 0x83.toByte()
		resolver.into(udp(upstream, 53, source, sourcePort(forwarded), failure))
		assertTrue(resolver.out(fromOs(question)) is DnsResolver.Outgoing.Query)
	}

	@Test
	fun spoofedRepliesAreDropped() {
		val resolver = resolver()
		val question = query(1)
		val forwarded = (resolver.out(fromOs(question)) as DnsResolver.Outgoing.Query).packet
		val port = sourcePort(forwarded)
		val stranger = InetAddress.getByName("6.6.6.6")
		assertSame(DnsResolver.Incoming.Drop, resolver.into(udp(stranger, 53, source, port, answer(question, 60))))
		assertSame(DnsResolver.Incoming.Drop, resolver.into(udp(upstream, 53, source, port + 1, answer(question, 60))))
	}

	@Test
	fun largeEdnsSizesAreLowered() {
		val resolver = resolver()
		val question = query(1).copyOf() + byteArrayOf(0, 0, 41, 0x10, 0, 0, 0, 0, 0, 0, 0)
		question[11] = 1
		val forwarded = (resolver.out(fromOs(question)) as DnsResolver.Outgoing.Query).packet
		val message = payload(forwarded)
		val at = message.size - 8
		assertEquals(1232, ((message[at].toInt() and 0xff) shl 8) or (message[at + 1].toInt() and 0xff))
	}

	@Test
	fun malformedQueriesDoNotCrash() {
		val resolver = resolver()
		assertSame(DnsResolver.Outgoing.Drop, resolver.out(fromOs(byteArrayOf(1, 2, 3))))
		val bad = byteArrayOf(0, 1, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 200.toByte())
		assertTrue(resolver.out(fromOs(bad)) is DnsResolver.Outgoing.Query)
	}

	/** The same packet as the Rust test's, so the two implementations agree on checksums. */
	@Test
	fun checksumsMatchTheRustImplementation() {
		val packet = DnsResolver.buildUdp(client.address, 1000, upstream.address, 53, "odd".toByteArray())
		var sum = 0
		for (index in 0 until 20 step 2) {
			sum += ((packet[index].toInt() and 0xff) shl 8) or (packet[index + 1].toInt() and 0xff)
		}
		while (sum ushr 16 != 0) sum = (sum and 0xffff) + (sum ushr 16)
		assertEquals(0xffff, sum)
	}
}
