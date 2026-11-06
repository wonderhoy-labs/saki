package com.wonderhoylabs.saki.link

import com.wonderhoylabs.saki.bluetooth.Link
import java.io.InputStream
import java.io.OutputStream
import java.net.Socket

/**
 * A link over plain TCP, which debug builds can use to test everything above Bluetooth on an
 * emulator, whose Bluetooth can't reach a real device. It has no pairing or encryption, so release
 * builds never offer it.
 */
class TcpLink(private val socket: Socket, override val peerName: String) : Link {
    override val input: InputStream get() = socket.getInputStream()
    override val output: OutputStream get() = socket.getOutputStream()

    override fun close() {
        runCatching { socket.close() }
    }
}
