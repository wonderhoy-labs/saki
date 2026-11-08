package com.wonderhoylabs.saki.server

import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.IBinder
import android.util.Log
import androidx.core.app.ServiceCompat
import com.wonderhoylabs.saki.BuildConfig
import com.wonderhoylabs.saki.ConnectedClient
import com.wonderhoylabs.saki.R
import com.wonderhoylabs.saki.SakiState
import com.wonderhoylabs.saki.ServerStatus
import com.wonderhoylabs.saki.bluetooth.Link
import com.wonderhoylabs.saki.bluetooth.ServerEndpoint
import com.wonderhoylabs.saki.client.statusNotification
import com.wonderhoylabs.saki.link.Frame
import com.wonderhoylabs.saki.link.FrameLink
import com.wonderhoylabs.saki.link.OutboundQueue
import com.wonderhoylabs.saki.link.Proto
import com.wonderhoylabs.saki.link.TcpLink
import java.io.IOException
import java.net.Inet4Address
import java.net.ServerSocket
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlin.concurrent.thread

/**
 * Server mode: shares this phone's network with clients. There's no VPN involved. Each client gets
 * a NAT that turns its packets into ordinary sockets.
 */
class ServerService : Service() {
	private var endpoint: ServerEndpoint? = null
	private var devListener: ServerSocket? = null
	private val sessions = ConcurrentHashMap<Long, Session>()
	private val nextId = AtomicLong()

	@Volatile private var sharingName: String? = null

	@Volatile private var blockLan = false

	private class Session(val client: ConnectedClient, val link: FrameLink, val transport: Link)

	override fun onBind(intent: Intent?): IBinder? = null

	override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
		if (intent?.action == ACTION_STOP) {
			shutDown()
			return START_NOT_STICKY
		}
		if (endpoint != null || devListener != null) return START_NOT_STICKY

		SakiState.setError(null)
		SakiState.setServer(ServerStatus.Starting)
		blockLan = intent?.getBooleanExtra(EXTRA_BLOCK_LAN, false) ?: false
		goForeground(getString(R.string.status_starting))

		val devPort = intent?.getIntExtra(EXTRA_DEV_TCP_PORT, 0)?.takeIf { BuildConfig.DEBUG && it != 0 }
		if (devPort != null) {
			startTcp(devPort)
		} else {
			val endpoint = ServerEndpoint(
				context = this,
				onReady = ::onReady,
				onClient = ::serve,
				onFailed = { message ->
					SakiState.setError(message)
					shutDown()
				},
			)
			this.endpoint = endpoint
			endpoint.start()
		}
		return START_NOT_STICKY
	}

	override fun onDestroy() {
		shutDown()
		super.onDestroy()
	}

	private fun onReady(name: String) {
		sharingName = name
		publish()
		goForeground(getString(R.string.status_sharing_none, name))
	}

	private fun startTcp(port: Int) {
		val listener = ServerSocket(port)
		devListener = listener
		onReady("TCP port $port")
		thread(name = "saki-dev-accept", isDaemon = true) {
			while (true) {
				val socket = try {
					listener.accept()
				} catch (_: IOException) {
					return@thread
				}
				socket.tcpNoDelay = true
				serve(TcpLink(socket, socket.remoteSocketAddress.toString()))
			}
		}
	}

	private fun serve(transport: Link) {
		thread(name = "saki-client", isDaemon = true) {
			val id = nextId.incrementAndGet()
			try {
				runSession(id, transport)
			} catch (error: IOException) {
				Log.i(TAG, "client ${transport.peerName}: ${error.message}")
			} finally {
				sessions.remove(id)
				transport.close()
				publish()
			}
		}
	}

	private fun runSession(id: Long, transport: Link) {
		val link = FrameLink(transport.input, transport.output)
		val welcome = Frame.Welcome.standard()
		link.serverHandshake(welcome)

		val nat = NatEngine(Proto.DEFAULT_DNS as Inet4Address, blockLan)
		val outbound = OutboundQueue()
		val natReader = thread(name = "saki-nat-reader", isDaemon = true) {
			val buffer = ByteArray(Proto.MTU + 64)
			while (true) {
				val length = nat.read(buffer)
				if (length < 0) return@thread
				outbound.offerPacket(buffer.copyOf(length))
			}
		}

		sessions[id] = Session(ConnectedClient(id, transport.peerName, link.stats), link, transport)
		publish()
		try {
			link.run(outbound, nat::input)
		} finally {
			nat.stop()
			natReader.join()
			nat.free()
		}
	}

	private fun publish() {
		val name = sharingName ?: return
		SakiState.setServer(ServerStatus.Sharing(name, sessions.values.map { it.client }.sortedBy { it.id }))
	}

	private fun goForeground(text: String) {
		val stopIntent = Intent(this, ServerService::class.java).setAction(ACTION_STOP)
		val notification = statusNotification(this, text, getString(R.string.notification_stop), stopIntent)
		ServiceCompat.startForeground(
			this,
			NOTIFICATION_ID,
			notification,
			ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE,
		)
	}

	private fun shutDown() {
		sharingName = null
		endpoint?.stop()
		endpoint = null
		devListener?.let { runCatching { it.close() } }
		devListener = null
		sessions.values.forEach { session ->
			session.link.stop()
			session.transport.close()
		}
		SakiState.setServer(ServerStatus.Stopped)
		ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
		stopSelf()
	}

	companion object {
		private const val TAG = "SakiServer"
		private const val ACTION_STOP = "com.wonderhoylabs.saki.STOP_SERVER"
		private const val EXTRA_BLOCK_LAN = "block_lan"
		private const val EXTRA_DEV_TCP_PORT = "dev_tcp_port"
		private const val NOTIFICATION_ID = 2

		fun start(context: Context, blockLan: Boolean) {
			val intent = Intent(context, ServerService::class.java).putExtra(EXTRA_BLOCK_LAN, blockLan)
			context.startForegroundService(intent)
		}

		/** Debug builds only: listens on TCP, to test on an emulator. */
		fun startOverTcp(context: Context, port: Int) {
			val intent = Intent(context, ServerService::class.java).putExtra(EXTRA_DEV_TCP_PORT, port)
			context.startForegroundService(intent)
		}

		fun stop(context: Context) {
			context.startService(Intent(context, ServerService::class.java).setAction(ACTION_STOP))
		}
	}
}
