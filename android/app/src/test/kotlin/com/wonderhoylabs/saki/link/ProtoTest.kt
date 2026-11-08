package com.wonderhoylabs.saki.link

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayInputStream
import java.net.InetAddress

/** The Rust side's proto tests check these same bytes. Change them together. */
class ProtoTest {
	private fun bytes(vararg values: Int) = ByteArray(values.size) { values[it].toByte() }

	private val welcomeBytes = bytes(
		2, 0, 48, // header
		0x05, 0x00, // mtu
		10, 233, 77, 2, // client v4
		10, 233, 77, 1, // gateway v4
		24, // prefix v4
		0xfd, 0x53, 0x61, 0x6b, 0x69, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, // client v6
		0xfd, 0x53, 0x61, 0x6b, 0x69, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, // gateway v6
		64, // prefix v6
		1, 1, 1, 1, // upstream dns
	)

	@Test
	fun framesMatchTheDocumentedBytes() {
		assertArrayEquals(bytes(1, 0, 5, 'S'.code, 'A'.code, 'K'.code, 'I'.code, 1), Frame.Hello(1).encode())
		assertArrayEquals(welcomeBytes, Frame.Welcome.standard().encode())
		assertArrayEquals(bytes(3, 0, 2, 0x45, 0x00), Frame.Packet(bytes(0x45, 0x00)).encode())
		assertArrayEquals(bytes(4, 0, 8, 1, 2, 3, 4, 5, 6, 7, 8), Frame.Ping(0x0102030405060708).encode())
		assertArrayEquals(bytes(5, 0, 8, 0, 0, 0, 0, 0, 0, 0, 9), Frame.Pong(9).encode())
		assertArrayEquals(bytes(6, 0, 1, 1), Frame.Bye(Proto.BYE_VERSION_MISMATCH).encode())
		assertArrayEquals(bytes('S'.code, 'A'.code, 'K'.code, 'I'.code, 1, 0x00, 0x81), Proto.encodeInfo(0x0081))
	}

	@Test
	fun framesSurviveARoundTrip() {
		val wire = Frame.Hello(Proto.VERSION).encode() +
			Frame.Welcome.standard(InetAddress.getByName("9.9.9.9")).encode() +
			Frame.Packet(ByteArray(1280) { 7 }).encode() +
			Frame.Ping(Long.MAX_VALUE).encode() +
			Frame.Pong(0).encode() +
			Frame.Bye(Proto.BYE_NORMAL).encode()
		val input = ByteArrayInputStream(wire)
		assertEquals(Frame.Hello(Proto.VERSION), Frame.read(input))
		assertEquals(Frame.Welcome.standard(InetAddress.getByName("9.9.9.9")), Frame.read(input))
		val packet = Frame.read(input) as Frame.Packet
		assertEquals(1280, packet.bytes.size)
		assertEquals(Frame.Ping(Long.MAX_VALUE), Frame.read(input))
		assertEquals(Frame.Pong(0), Frame.read(input))
		assertEquals(Frame.Bye(Proto.BYE_NORMAL), Frame.read(input))
	}

	@Test
	fun unknownFramesAreSkipped() {
		val input = ByteArrayInputStream(bytes(99, 0, 2, 1, 2, 4, 0, 8, 0, 0, 0, 0, 0, 0, 0, 5))
		assertEquals(Frame.Ping(5), Frame.read(input))
	}

	@Test
	fun aBadHelloIsAnError() {
		val input = ByteArrayInputStream(bytes(1, 0, 5, 'N'.code, 'O'.code, 'P'.code, 'E'.code, 1))
		assertThrows(java.io.IOException::class.java) { Frame.read(input) }
	}

	@Test
	fun welcomeAcceptsTrailingFields() {
		val payload = welcomeBytes.copyOfRange(3, welcomeBytes.size) + bytes(0xff)
		assertEquals(Frame.Welcome.standard(), Frame.decode(2, payload))
	}

	@Test
	fun infoRoundTripsAndRejectsGarbage() {
		assertEquals(Proto.VERSION to 0x1234, Proto.decodeInfo(Proto.encodeInfo(0x1234)))
		assertThrows(java.io.IOException::class.java) { Proto.decodeInfo("SAKI".toByteArray()) }
		assertThrows(java.io.IOException::class.java) {
			Proto.decodeInfo("NOPE".toByteArray() + bytes(1, 0, 0x81))
		}
	}

	@Test
	fun uuidsMatchTheProtocol() {
		assertTrue(Proto.SERVICE_UUID.startsWith("53414b49"))
	}
}
