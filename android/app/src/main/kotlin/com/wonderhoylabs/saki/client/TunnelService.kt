package com.wonderhoylabs.saki.client

import android.app.Service
import android.bluetooth.BluetoothDevice
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.VpnService
import android.os.Build
import android.os.ParcelFileDescriptor
import android.system.ErrnoException
import android.system.Os
import android.system.OsConstants
import android.system.StructPollfd
import android.util.Log
import androidx.core.app.ServiceCompat
import com.wonderhoylabs.saki.BuildConfig
import com.wonderhoylabs.saki.ClientStatus
import com.wonderhoylabs.saki.R
import com.wonderhoylabs.saki.SakiState
import com.wonderhoylabs.saki.bluetooth.ClientLink
import com.wonderhoylabs.saki.bluetooth.FoundServer
import com.wonderhoylabs.saki.bluetooth.Link
import com.wonderhoylabs.saki.bluetooth.ServerScanner
import com.wonderhoylabs.saki.bluetooth.hasBluetoothPermissions
import com.wonderhoylabs.saki.link.DnsResolver
import com.wonderhoylabs.saki.link.Frame
import com.wonderhoylabs.saki.link.FrameLink
import com.wonderhoylabs.saki.link.OutboundQueue
import com.wonderhoylabs.saki.link.Proto
import com.wonderhoylabs.saki.link.TcpLink
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlin.concurrent.thread

/**
 * Client mode: a VPN that carries all of this phone's traffic over Bluetooth to a server.
 *
 * The tunnel stays up when the link drops, so traffic waits for a reconnection instead of escaping
 * over another network, and apps keep their routes.
 */
class TunnelService : VpnService() {
    private var worker: Thread? = null
    @Volatile private var stopping = false
    private val currentLink = AtomicReference<FrameLink?>()
    private val currentTransport = AtomicReference<Link?>()

    private val outbound = OutboundQueue()
    private var tunnel: ParcelFileDescriptor? = null
    private var tunReader: Thread? = null
    private var wakePipe: Array<ParcelFileDescriptor>? = null
    private var resolver: DnsResolver? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopTunnel()
            return Service.START_NOT_STICKY
        }
        if (worker != null) return Service.START_NOT_STICKY

        val device = intent?.getParcelableExtraCompat<BluetoothDevice>(EXTRA_DEVICE)
        val name = intent?.getStringExtra(EXTRA_NAME) ?: "the server"
        val devTcp = intent?.getStringExtra(EXTRA_DEV_TCP)?.takeIf { BuildConfig.DEBUG }
        if (device == null && devTcp == null) {
            stopSelf()
            return Service.START_NOT_STICKY
        }

        SakiState.setError(null)
        SakiState.setClient(ClientStatus.Connecting(name))
        goForeground(getString(R.string.status_connecting, name))
        stopping = false
        worker = thread(name = "saki-tunnel") { runTunnel(device, name, devTcp) }
        return Service.START_NOT_STICKY
    }

    override fun onRevoke() {
        // Another VPN took over, or the user revoked this one in Settings.
        stopTunnel()
        super.onRevoke()
    }

    override fun onDestroy() {
        stopTunnel()
        super.onDestroy()
    }

    private fun stopTunnel() {
        if (stopping) return
        stopping = true
        currentLink.get()?.stop()
        currentTransport.get()?.close()
        worker?.interrupt()
    }

    private fun goForeground(text: String) {
        val stopIntent = Intent(this, TunnelService::class.java).setAction(ACTION_STOP)
        val notification = statusNotification(this, text, getString(R.string.notification_disconnect), stopIntent)
        ServiceCompat.startForeground(this, NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
    }

    private fun runTunnel(device: BluetoothDevice?, name: String, devTcp: String?) {
        var retryMillis = FIRST_RETRY_MILLIS
        var bestDevice = device
        try {
            while (!stopping) {
                try {
                    val transport = openTransport(bestDevice, name, devTcp)
                    currentTransport.set(transport)
                    val link = FrameLink(transport.input, transport.output)
                    val welcome = link.clientHandshake()
                    currentLink.set(link)
                    if (stopping) break
                    ensureTunnel(welcome)
                    retryMillis = FIRST_RETRY_MILLIS
                    outbound.clear()
                    SakiState.setClient(ClientStatus.Connected(transport.peerName, link.stats))
                    SakiState.setError(null)
                    goForeground(getString(R.string.status_connected, transport.peerName))
                    link.run(outbound, ::deliverToSystem)
                    // A clean goodbye from the server still means there's no link now.
                    if (!stopping) SakiState.setClient(ClientStatus.Reconnecting(name, "$name closed the connection."))
                } catch (_: InterruptedException) {
                    // Only stopTunnel interrupts this thread.
                    break
                } catch (error: IOException) {
                    if (stopping) break
                    Log.i(TAG, "link failed: ${error.message}")
                    SakiState.setClient(ClientStatus.Reconnecting(name, error.message))
                    goForeground(getString(R.string.status_reconnecting, name))
                } finally {
                    currentLink.set(null)
                    currentTransport.getAndSet(null)?.close()
                }
                if (stopping) break
                try {
                    Thread.sleep(retryMillis)
                } catch (_: InterruptedException) {
                    break
                }
                retryMillis = (retryMillis * 2).coerceAtMost(MAX_RETRY_MILLIS)
            }
        } catch (error: SecurityException) {
            SakiState.setError("Saki lost its Bluetooth permission.")
        } finally {
            // The interrupt has done its job, and it would only get in the way of cleaning up.
            Thread.interrupted()
            closeTunnel()
            SakiState.setClient(ClientStatus.Idle)
            worker = null
            ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
            stopSelf()
        }
    }

    private fun openTransport(device: BluetoothDevice?, name: String, devTcp: String?): Link {
        if (devTcp != null) {
            val host = devTcp.substringBeforeLast(':')
            val port = devTcp.substringAfterLast(':').toInt()
            val socket = Socket()
            // A VPN's own socket mustn't go through the VPN. Java only creates the real socket once
            // it's bound, and before that this would have nothing to act on.
            socket.bind(null)
            if (!protect(socket)) throw IOException("Couldn't keep the link out of the VPN.")
            socket.tcpNoDelay = true
            socket.connect(InetSocketAddress(host, port), DEV_CONNECT_TIMEOUT_MILLIS)
            return TcpLink(socket, devTcp)
        }
        if (!hasBluetoothPermissions(this)) throw IOException("Saki needs its Bluetooth permission back.")
        // Addresses change over time unless the devices have paired, so look again each attempt.
        val found = findAgain(device, name)
        return ClientLink.open(this, found ?: device!!, name)
    }

    /** Looks for the server's current address, by its old one or by its name. */
    private fun findAgain(device: BluetoothDevice?, name: String): BluetoothDevice? {
        val result = AtomicReference<FoundServer?>()
        val done = CountDownLatch(1)
        val scanner = ServerScanner(this, onFound = { server ->
            if (server.address == device?.address || server.name == name) {
                result.compareAndSet(null, server)
                done.countDown()
            }
        })
        scanner.start()
        try {
            done.await(FIND_TIMEOUT_SECONDS, TimeUnit.SECONDS)
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
        } finally {
            scanner.stop()
        }
        return result.get()?.device
    }

    private fun ensureTunnel(welcome: Frame.Welcome) {
        resolver?.setUpstream(welcome.upstreamDns)
        if (tunnel != null) return

        resolver = DnsResolver(welcome.gatewayV4, Proto.RESOLVER_V4, welcome.upstreamDns)
        val builder = Builder()
            .setSession(getString(R.string.app_name))
            .setMtu(welcome.mtu)
            .addAddress(welcome.clientV4, welcome.prefixV4)
            .addAddress(welcome.clientV6, welcome.prefixV6)
            .addRoute("0.0.0.0", 0)
            // Without this, IPv6 would slip past the tunnel on networks that have it.
            .addRoute("::", 0)
            .addDnsServer(welcome.gatewayV4)
            .setBlocking(false)
            .setMetered(false)
        val established = builder.establish() ?: throw IOException("Android didn't allow the VPN. Open Saki and try again.")
        tunnel = established

        val pipe = ParcelFileDescriptor.createPipe()
        wakePipe = pipe
        tunReader = thread(name = "saki-tun-reader", isDaemon = true) { readFromSystem(established, pipe[0]) }
        markAsConnected()
    }

    /**
     * With no other network, Android would treat the VPN as having nothing underneath it and tell
     * apps they're offline. Naming the VPN as its own underlying network tells them otherwise.
     */
    private fun markAsConnected() {
        val connectivity = getSystemService(ConnectivityManager::class.java)
        @Suppress("DEPRECATION")
        val vpn = connectivity.allNetworks.firstOrNull { network ->
            connectivity.getNetworkCapabilities(network)?.hasTransport(NetworkCapabilities.TRANSPORT_VPN) == true
        }
        if (vpn != null) setUnderlyingNetworks(arrayOf(vpn))
    }

    /** Moves packets from Android towards the link until the tunnel closes. */
    private fun readFromSystem(tunnel: ParcelFileDescriptor, wake: ParcelFileDescriptor) {
        val input = FileInputStream(tunnel.fileDescriptor)
        val buffer = ByteArray(Proto.MTU + 64)
        val tunPoll = StructPollfd().apply {
            fd = tunnel.fileDescriptor
            events = OsConstants.POLLIN.toShort()
        }
        val wakePoll = StructPollfd().apply {
            fd = wake.fileDescriptor
            events = OsConstants.POLLIN.toShort()
        }
        val polls = arrayOf(tunPoll, wakePoll)
        while (true) {
            try {
                Os.poll(polls, -1)
            } catch (error: ErrnoException) {
                if (error.errno == OsConstants.EINTR) continue
                return
            }
            if (wakePoll.revents.toInt() != 0 || stopping) return
            val length = try {
                input.read(buffer)
            } catch (error: IOException) {
                val cause = error.cause as? ErrnoException
                if (cause?.errno == OsConstants.EAGAIN) continue
                return
            }
            if (length <= 0) continue
            val resolver = resolver ?: continue
            when (val outcome = resolver.outgoing(buffer, length)) {
                is DnsResolver.Outgoing.Pass -> outbound.offerPacket(buffer.copyOf(length))
                is DnsResolver.Outgoing.Query -> outbound.offerPacket(outcome.packet)
                is DnsResolver.Outgoing.Reply -> writeToSystem(outcome.packet)
                is DnsResolver.Outgoing.Drop -> {}
            }
        }
    }

    /** Called with each packet from the link. */
    private fun deliverToSystem(packet: ByteArray) {
        when (val outcome = resolver?.incoming(packet, packet.size) ?: return) {
            is DnsResolver.Incoming.Pass -> writeToSystem(packet)
            is DnsResolver.Incoming.Reply -> writeToSystem(outcome.packet)
            is DnsResolver.Incoming.Drop -> {}
        }
    }

    private val systemOutput: FileOutputStream? by lazy(LazyThreadSafetyMode.PUBLICATION) {
        tunnel?.let { FileOutputStream(it.fileDescriptor) }
    }

    private fun writeToSystem(packet: ByteArray) {
        try {
            systemOutput?.write(packet)
        } catch (_: IOException) {
            // The tunnel is closing, or its queue is full. Either way, this packet is lost.
        }
    }

    private fun closeTunnel() {
        // Writing to this wakes the reader, which can't be interrupted any other way.
        wakePipe?.let { pipe ->
            runCatching {
                ParcelFileDescriptor.AutoCloseOutputStream(pipe[1]).use { it.write(0) }
            }
        }
        tunReader?.join(READER_JOIN_MILLIS)
        wakePipe?.getOrNull(0)?.let { runCatching { it.close() } }
        wakePipe = null
        tunnel?.let { runCatching { it.close() } }
        tunnel = null
        resolver = null
    }

    companion object {
        private const val TAG = "SakiTunnel"
        private const val ACTION_STOP = "com.wonderhoylabs.saki.STOP_TUNNEL"
        private const val EXTRA_DEVICE = "device"
        private const val EXTRA_NAME = "name"
        private const val EXTRA_DEV_TCP = "dev_tcp"
        private const val NOTIFICATION_ID = 1
        private const val FIRST_RETRY_MILLIS = 1_000L
        private const val MAX_RETRY_MILLIS = 15_000L
        private const val FIND_TIMEOUT_SECONDS = 10L
        private const val DEV_CONNECT_TIMEOUT_MILLIS = 5_000
        private const val READER_JOIN_MILLIS = 1_000L

        fun connect(context: Context, server: FoundServer) {
            val intent = Intent(context, TunnelService::class.java)
                .putExtra(EXTRA_DEVICE, server.device)
                .putExtra(EXTRA_NAME, server.name)
            context.startForegroundService(intent)
        }

        /** Debug builds only: connects over TCP, to test on an emulator. */
        fun connectOverTcp(context: Context, address: String) {
            val intent = Intent(context, TunnelService::class.java)
                .putExtra(EXTRA_DEV_TCP, address)
                .putExtra(EXTRA_NAME, address)
            context.startForegroundService(intent)
        }

        fun disconnect(context: Context) {
            context.startService(Intent(context, TunnelService::class.java).setAction(ACTION_STOP))
        }

        private inline fun <reified T : android.os.Parcelable> Intent.getParcelableExtraCompat(name: String): T? =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                getParcelableExtra(name, T::class.java)
            } else {
                @Suppress("DEPRECATION")
                getParcelableExtra(name)
            }
    }
}
