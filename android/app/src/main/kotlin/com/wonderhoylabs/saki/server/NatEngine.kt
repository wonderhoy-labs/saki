package com.wonderhoylabs.saki.server

import java.net.Inet4Address
import java.nio.ByteBuffer

/**
 * One client's NAT, which is the Rust code from `crates/saki-core`, also used by the command-line
 * server. See `crates/saki-jni` for the other side of these calls.
 */
class NatEngine(upstreamDns: Inet4Address, blockLan: Boolean) {
    private var handle = nativeStart(ByteBuffer.wrap(upstreamDns.address).int, blockLan)

    init {
        check(handle != 0L) { "Couldn't start the NAT." }
    }

    /** Hands the NAT an IP packet from the client. */
    fun input(packet: ByteArray) = nativeInput(handle, packet, packet.size)

    /** Waits for a packet for the client, and returns its length, or -1 once stopped. */
    fun read(buffer: ByteArray): Int = nativeRead(handle, buffer)

    /** Ends the client's connections. A thread waiting in [read] gets -1. */
    fun stop() = nativeStop(handle)

    /** Releases everything. Only call this once [stop] has returned and nothing's reading. */
    fun free() {
        nativeFree(handle)
        handle = 0
    }

    private companion object {
        init {
            System.loadLibrary("saki_jni")
        }

        @JvmStatic external fun nativeStart(upstreamDns: Int, blockLan: Boolean): Long

        @JvmStatic external fun nativeInput(handle: Long, packet: ByteArray, length: Int)

        @JvmStatic external fun nativeRead(handle: Long, buffer: ByteArray): Int

        @JvmStatic external fun nativeStop(handle: Long)

        @JvmStatic external fun nativeFree(handle: Long)
    }
}
