package com.wonderhoylabs.saki.link

import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.util.ArrayDeque
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.locks.ReentrantLock
import kotlin.concurrent.thread
import kotlin.concurrent.withLock

/** Counters for one link, safe to read from anywhere. */
class LinkStats {
    val bytesSent = AtomicLong()
    val bytesReceived = AtomicLong()
    /** The latest ping time in milliseconds, or zero before the first reply. */
    val rttMillis = AtomicInteger()
}

/**
 * Packets waiting to go out over the link. Bluetooth is slow enough that a longer queue only adds
 * delay, so once it's full new packets are dropped and TCP backs off, as with a real network
 * interface. Control frames skip the queue, since they measure the link and keep it alive.
 */
class OutboundQueue(private val capacity: Int = 32) {
    private val lock = ReentrantLock()
    private val notEmpty = lock.newCondition()
    private val packets = ArrayDeque<ByteArray>()
    private val control = ArrayDeque<Frame>()

    /** Returns false if the packet was dropped. */
    fun offerPacket(packet: ByteArray): Boolean = lock.withLock {
        if (packets.size >= capacity) return false
        packets.addLast(packet)
        notEmpty.signal()
        true
    }

    fun offerControl(frame: Frame): Unit = lock.withLock {
        control.addLast(frame)
        notEmpty.signal()
    }

    fun clear(): Unit = lock.withLock {
        packets.clear()
        control.clear()
    }

    /** Waits for something to send, or returns null after the timeout. */
    fun take(timeoutMillis: Long): Frame? = lock.withLock {
        var remaining = java.util.concurrent.TimeUnit.MILLISECONDS.toNanos(timeoutMillis)
        while (control.isEmpty() && packets.isEmpty()) {
            if (remaining <= 0) return null
            remaining = notEmpty.awaitNanos(remaining)
        }
        pollLocked()
    }

    /** Returns what's next without waiting. */
    fun poll(): Frame? = lock.withLock { pollLocked() }

    private fun pollLocked(): Frame? =
        control.pollFirst() ?: packets.pollFirst()?.let { Frame.Packet(it) }

    /** Wakes up a thread waiting in [take]. */
    fun wake(): Unit = lock.withLock { notEmpty.signalAll() }
}

/**
 * Runs the protocol over a byte stream: the handshake, then frames in both directions at once
 * until the link ends.
 */
class FrameLink(private val input: InputStream, private val output: OutputStream) {
    val stats = LinkStats()
    private val stopping = AtomicBoolean(false)

    /** Runs the client's half of the handshake. */
    fun clientHandshake(): Frame.Welcome {
        output.write(Frame.Hello(Proto.VERSION).encode())
        output.flush()
        return when (val reply = Frame.read(input)) {
            is Frame.Welcome -> reply
            is Frame.Bye -> throw IOException(
                if (reply.reason == Proto.BYE_VERSION_MISMATCH) {
                    "The server speaks a different protocol version. Update the older one."
                } else {
                    "The server turned this connection away."
                },
            )
            else -> throw IOException("Expected a welcome from the server.")
        }
    }

    /** Runs the server's half of the handshake. */
    fun serverHandshake(welcome: Frame.Welcome) {
        val hello = Frame.read(input) as? Frame.Hello ?: throw IOException("Expected a hello.")
        if (hello.version != Proto.VERSION) {
            output.write(Frame.Bye(Proto.BYE_VERSION_MISMATCH).encode())
            output.flush()
            throw IOException("The client speaks protocol ${hello.version}, and this is ${Proto.VERSION}.")
        }
        output.write(welcome.encode())
        output.flush()
    }

    /**
     * Carries [outbound] packets to the peer and hands arriving ones to [onPacket], which must not
     * block. Returns when the peer says goodbye or [stop] is called, and throws if the link fails
     * or goes quiet.
     */
    fun run(outbound: OutboundQueue, onPacket: (ByteArray) -> Unit) {
        val lastHeard = AtomicLong(System.nanoTime())
        val failure = java.util.concurrent.atomic.AtomicReference<Throwable?>()
        val writer = thread(name = "saki-link-writer", isDaemon = true) {
            try {
                writeLoop(outbound, lastHeard)
            } catch (error: Throwable) {
                failure.compareAndSet(null, error)
                // Reading can't be interrupted, so closing the stream is how it hears about this.
                closeQuietly()
            }
        }
        this.outbound = outbound
        try {
            readLoop(outbound, lastHeard, onPacket)
        } catch (error: IOException) {
            if (!stopping.get()) failure.compareAndSet(null, error)
        } finally {
            stopping.set(true)
            outbound.wake()
        }
        try {
            writer.join(WRITER_JOIN_MILLIS)
        } catch (_: InterruptedException) {
            // Whoever interrupted this wants it to stop, and that's already happening.
            Thread.currentThread().interrupt()
        }
        failure.get()?.let { error ->
            throw if (error is IOException) error else IOException(error)
        }
    }

    @Volatile private var outbound: OutboundQueue? = null

    /** Ends the link from this side, telling the peer. */
    fun stop() {
        if (stopping.compareAndSet(false, true)) {
            outbound?.wake()
        }
    }

    private fun readLoop(outbound: OutboundQueue, lastHeard: AtomicLong, onPacket: (ByteArray) -> Unit) {
        val epoch = this.epoch
        while (!stopping.get()) {
            val frame = Frame.read(input)
            lastHeard.set(System.nanoTime())
            when (frame) {
                is Frame.Packet -> {
                    stats.bytesReceived.addAndGet(frame.length.toLong())
                    onPacket(frame.bytes)
                }
                is Frame.Ping -> outbound.offerControl(Frame.Pong(frame.nonce))
                is Frame.Pong -> {
                    val now = (System.nanoTime() - epoch) / NANOS_PER_MILLI
                    stats.rttMillis.set((now - frame.nonce).coerceIn(1, Int.MAX_VALUE.toLong()).toInt())
                }
                is Frame.Bye -> return
                is Frame.Hello, is Frame.Welcome -> {}
            }
        }
    }

    private val epoch = System.nanoTime()

    private fun writeLoop(outbound: OutboundQueue, lastHeard: AtomicLong) {
        val batch = ByteArrayOutputStream(WRITE_BATCH_BYTES * 2)
        var nextPing = System.nanoTime()
        while (!stopping.get()) {
            val now = System.nanoTime()
            if (now - lastHeard.get() > LINK_TIMEOUT_NANOS) throw IOException("The link went quiet.")
            if (now - nextPing >= 0) {
                outbound.offerControl(Frame.Ping((now - epoch) / NANOS_PER_MILLI))
                nextPing = now + PING_INTERVAL_NANOS
            }
            val waitMillis = ((nextPing - now) / NANOS_PER_MILLI).coerceAtLeast(1)
            var frame = outbound.take(waitMillis) ?: continue
            // Take whatever else is ready, so a burst of packets goes out as one write.
            while (true) {
                if (frame is Frame.Packet) stats.bytesSent.addAndGet(frame.length.toLong())
                batch.write(frame.encode())
                if (batch.size() >= WRITE_BATCH_BYTES) break
                frame = outbound.poll() ?: break
            }
            batch.writeTo(output)
            output.flush()
            batch.reset()
        }
        try {
            output.write(Frame.Bye(Proto.BYE_NORMAL).encode())
            output.flush()
        } catch (_: IOException) {
            // It was only a courtesy, and the link is ending anyway.
        }
        closeQuietly()
    }

    private fun closeQuietly() {
        try {
            input.close()
        } catch (_: IOException) {
        }
        try {
            output.close()
        } catch (_: IOException) {
        }
    }

    private companion object {
        const val NANOS_PER_MILLI = 1_000_000L
        const val PING_INTERVAL_NANOS = 10_000_000_000L
        const val LINK_TIMEOUT_NANOS = 35_000_000_000L
        const val WRITE_BATCH_BYTES = 8 * 1024
        const val WRITER_JOIN_MILLIS = 1500L
    }
}
