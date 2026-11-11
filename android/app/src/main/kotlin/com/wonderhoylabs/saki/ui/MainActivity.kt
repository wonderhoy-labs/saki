package com.wonderhoylabs.saki.ui

import android.annotation.SuppressLint
import android.app.Activity
import android.bluetooth.BluetoothAdapter
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.net.VpnService
import android.os.Bundle
import android.text.format.Formatter
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.Image
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.LifecycleEventObserver
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.wonderhoylabs.saki.BuildConfig
import com.wonderhoylabs.saki.ClientStatus
import com.wonderhoylabs.saki.R
import com.wonderhoylabs.saki.SakiState
import com.wonderhoylabs.saki.ServerStatus
import com.wonderhoylabs.saki.bluetooth.FoundServer
import com.wonderhoylabs.saki.bluetooth.ServerScanner
import com.wonderhoylabs.saki.bluetooth.bluetoothAdapter
import com.wonderhoylabs.saki.bluetooth.bluetoothPermissions
import com.wonderhoylabs.saki.bluetooth.hasBluetoothPermissions
import com.wonderhoylabs.saki.client.TunnelService
import com.wonderhoylabs.saki.link.LinkStats
import com.wonderhoylabs.saki.server.ServerService
import kotlinx.coroutines.delay

class MainActivity : ComponentActivity() {
	override fun onCreate(savedInstanceState: Bundle?) {
		enableEdgeToEdge()
		super.onCreate(savedInstanceState)
		setContent {
			SakiTheme {
				Surface(modifier = Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
					SakiScreen(devIntent = intent.takeIf { BuildConfig.DEBUG })
				}
			}
		}
	}

	companion object {
		/** Debug builds only: `--es dev_tcp host:port` connects, for testing on an emulator. */
		const val EXTRA_DEV_TCP = "dev_tcp"

		/** Debug builds only: `--ei dev_tcp_listen port` starts a server, for the same reason. */
		const val EXTRA_DEV_TCP_LISTEN = "dev_tcp_listen"

		/** Debug builds only: `--ez dev_share true` starts sharing over Bluetooth, on a phone nobody's holding. */
		const val EXTRA_DEV_SHARE = "dev_share"
	}
}

private enum class Role { Client, Server }

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SakiScreen(devIntent: Intent?) {
	val context = LocalContext.current
	val client by SakiState.client.collectAsStateWithLifecycle()
	val server by SakiState.server.collectAsStateWithLifecycle()
	val error by SakiState.error.collectAsStateWithLifecycle()
	var role by rememberSaveable { mutableStateOf(Role.Client) }
	var hasPermissions by remember { mutableStateOf(hasBluetoothPermissions(context)) }
	val bluetoothOn = rememberBluetoothOn()
	var pendingServer by remember { mutableStateOf<FoundServer?>(null) }
	var pendingDevTcp by remember { mutableStateOf<String?>(null) }

	val permissionLauncher = rememberLauncherForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) {
		hasPermissions = hasBluetoothPermissions(context)
	}
	val enableBluetooth = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) {}
	val vpnConsent = rememberLauncherForActivityResult(ActivityResultContracts.StartActivityForResult()) { result ->
		if (result.resultCode == Activity.RESULT_OK) {
			pendingServer?.let { TunnelService.connect(context, it) }
			pendingDevTcp?.let { TunnelService.connectOverTcp(context, it) }
		}
		pendingServer = null
		pendingDevTcp = null
	}

	fun connect(server: FoundServer) {
		val consent = VpnService.prepare(context)
		if (consent == null) {
			TunnelService.connect(context, server)
		} else {
			pendingServer = server
			vpnConsent.launch(consent)
		}
	}

	LaunchedEffect(Unit) {
		val devTcp = devIntent?.getStringExtra(MainActivity.EXTRA_DEV_TCP)
		val devListen = devIntent?.getIntExtra(MainActivity.EXTRA_DEV_TCP_LISTEN, 0) ?: 0
		if (devTcp != null) {
			val consent = VpnService.prepare(context)
			if (consent == null) {
				TunnelService.connectOverTcp(context, devTcp)
			} else {
				pendingDevTcp = devTcp
				vpnConsent.launch(consent)
			}
		} else if (devListen != 0) {
			role = Role.Server
			ServerService.startOverTcp(context, devListen)
		} else if (devIntent?.getBooleanExtra(MainActivity.EXTRA_DEV_SHARE, false) == true) {
			role = Role.Server
			ServerService.start(context, blockLan = false)
		}
	}

	Column(
		modifier = Modifier
			.fillMaxSize()
			.safeDrawingPadding()
			.verticalScroll(rememberScrollState())
			.padding(horizontal = 20.dp, vertical = 16.dp),
		verticalArrangement = Arrangement.spacedBy(16.dp),
	) {
		BrandHeader(subtitle = stringResource(R.string.tagline), logo = painterResource(R.drawable.logo))

		SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
			Role.entries.forEachIndexed { index, entry ->
				SegmentedButton(
					selected = role == entry,
					onClick = { role = entry },
					shape = SegmentedButtonDefaults.itemShape(index, Role.entries.size),
				) {
					Text(stringResource(if (entry == Role.Client) R.string.role_client else R.string.role_server))
				}
			}
		}

		error?.let { message -> Notice(message) }

		when {
			!hasPermissions -> ActionCard(
				text = stringResource(R.string.permissions_needed),
				action = stringResource(R.string.permissions_grant),
				onClick = { permissionLauncher.launch(bluetoothPermissions()) },
			)

			!bluetoothOn -> ActionCard(
				text = stringResource(R.string.bluetooth_off),
				action = stringResource(R.string.bluetooth_turn_on),
				onClick = { enableBluetooth.launch(Intent(BluetoothAdapter.ACTION_REQUEST_ENABLE)) },
			)

			role == Role.Client -> ClientPanel(client, onConnect = ::connect)

			else -> ServerPanel(server)
		}
	}
}

@Composable
private fun ClientPanel(status: ClientStatus, onConnect: (FoundServer) -> Unit) {
	val context = LocalContext.current
	when (status) {
		is ClientStatus.Idle -> {
			ServerList(onConnect)
		}

		is ClientStatus.Connecting -> StatusCard(
			stringResource(R.string.status_connecting, status.server),
			busy = true,
		) {
			DisconnectButton(context)
		}

		is ClientStatus.Reconnecting -> StatusCard(
			stringResource(R.string.status_reconnecting, status.server),
			busy = true,
		) {
			status.reason?.let { reason -> Text(reason, style = MaterialTheme.typography.bodySmall) }
			DisconnectButton(context)
		}

		is ClientStatus.Connected -> StatusCard(
			stringResource(R.string.status_connected, status.server),
			busy = false,
		) {
			StatsRow(status.stats)
			DisconnectButton(context)
		}
	}
}

@Composable
private fun DisconnectButton(context: Context) {
	FilledTonalButton(onClick = { TunnelService.disconnect(context) }, modifier = Modifier.fillMaxWidth()) {
		Text(stringResource(R.string.disconnect))
	}
}

@SuppressLint("MissingPermission") // The panel only shows once permission has been granted.
@Composable
private fun ServerList(onConnect: (FoundServer) -> Unit) {
	val context = LocalContext.current
	val servers = remember { mutableStateMapOf<String, FoundServer>() }
	val lifecycleOwner = LocalLifecycleOwner.current

	DisposableEffect(lifecycleOwner) {
		val scanner = ServerScanner(context, onFound = { server -> servers[server.address] = server })
		val observer = LifecycleEventObserver { _, event ->
			when (event) {
				Lifecycle.Event.ON_RESUME -> scanner.start()
				Lifecycle.Event.ON_PAUSE -> scanner.stop()
				else -> {}
			}
		}
		lifecycleOwner.lifecycle.addObserver(observer)
		onDispose {
			lifecycleOwner.lifecycle.removeObserver(observer)
			scanner.stop()
		}
	}

	Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
		Row(verticalAlignment = Alignment.CenterVertically) {
			Text(
				stringResource(R.string.servers_nearby),
				style = MaterialTheme.typography.titleMedium,
				modifier = Modifier.weight(1f),
			)
			CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
		}
		if (servers.isEmpty()) {
			Text(stringResource(R.string.servers_looking), style = MaterialTheme.typography.bodyMedium)
		}
		servers.values.sortedByDescending { it.rssi }.forEach { server ->
			Card(
				modifier = Modifier.fillMaxWidth().clip(RoundedCornerShape(16.dp)).clickable { onConnect(server) },
				colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.secondaryContainer),
				shape = RoundedCornerShape(16.dp),
			) {
				Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
					Column(Modifier.weight(1f)) {
						Text(
							server.name,
							style = MaterialTheme.typography.titleMedium,
							maxLines = 1,
							overflow = TextOverflow.Ellipsis,
						)
						Text(server.address, style = MaterialTheme.typography.bodySmall)
					}
					Text("${server.rssi} dBm", style = MaterialTheme.typography.labelMedium)
				}
			}
		}
	}
}

@Composable
private fun ServerPanel(status: ServerStatus) {
	val context = LocalContext.current
	var blockLan by rememberSaveable { mutableStateOf(false) }
	when (status) {
		is ServerStatus.Stopped -> {
			StatusCard(stringResource(R.string.status_not_sharing), busy = false) {
				Row(verticalAlignment = Alignment.CenterVertically) {
					Text(stringResource(R.string.block_lan), modifier = Modifier.weight(1f))
					Switch(checked = blockLan, onCheckedChange = { blockLan = it })
				}
				Text(stringResource(R.string.sharing_warning), style = MaterialTheme.typography.bodySmall)
				Button(onClick = { ServerService.start(context, blockLan) }, modifier = Modifier.fillMaxWidth()) {
					Text(stringResource(R.string.start_sharing))
				}
			}
		}

		is ServerStatus.Starting -> StatusCard(stringResource(R.string.status_starting), busy = true) {}

		is ServerStatus.Sharing -> {
			val title = if (status.clients.isEmpty()) {
				stringResource(R.string.status_sharing_none, status.name)
			} else {
				stringResource(R.string.status_sharing_some, status.name)
			}
			StatusCard(title, busy = status.clients.isEmpty()) {
				if (status.clients.isEmpty()) {
					Text(stringResource(R.string.sharing_pairing_hint), style = MaterialTheme.typography.bodySmall)
				}
				status.clients.forEach { client ->
					Column {
						Text(client.name, style = MaterialTheme.typography.titleSmall)
						StatsRow(client.stats)
					}
				}
				FilledTonalButton(onClick = { ServerService.stop(context) }, modifier = Modifier.fillMaxWidth()) {
					Text(stringResource(R.string.stop_sharing))
				}
			}
		}
	}
}

@Composable
private fun StatusCard(title: String, busy: Boolean, content: @Composable () -> Unit) {
	Card(
		modifier = Modifier.fillMaxWidth(),
		colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.primaryContainer),
		shape = RoundedCornerShape(20.dp),
	) {
		Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
			Row(verticalAlignment = Alignment.CenterVertically) {
				Text(title, style = MaterialTheme.typography.titleMedium, modifier = Modifier.weight(1f))
				if (busy) {
					Spacer(Modifier.width(12.dp))
					CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
				}
			}
			content()
		}
	}
}

@Composable
private fun StatsRow(stats: LinkStats) {
	val context = LocalContext.current
	var tick by remember { mutableIntStateOf(0) }
	LaunchedEffect(stats) {
		while (true) {
			delay(1000)
			tick++
		}
	}
	// Reading tick is what redraws this every second.
	val rtt = remember(tick) { stats.rttMillis.get() }
	val down = remember(tick) { Formatter.formatShortFileSize(context, stats.bytesReceived.get()) }
	val up = remember(tick) { Formatter.formatShortFileSize(context, stats.bytesSent.get()) }
	Row(horizontalArrangement = Arrangement.spacedBy(16.dp)) {
		Text(stringResource(R.string.stats_down, down), style = MaterialTheme.typography.bodyMedium)
		Text(stringResource(R.string.stats_up, up), style = MaterialTheme.typography.bodyMedium)
		if (rtt > 0) Text(stringResource(R.string.stats_ping, rtt), style = MaterialTheme.typography.bodyMedium)
	}
}

@Composable
private fun ActionCard(text: String, action: String, onClick: () -> Unit) {
	Card(
		modifier = Modifier.fillMaxWidth(),
		colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.tertiaryContainer),
		shape = RoundedCornerShape(20.dp),
	) {
		Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
			Text(text, style = MaterialTheme.typography.bodyLarge)
			Button(onClick = onClick, modifier = Modifier.fillMaxWidth()) { Text(action) }
		}
	}
}

@Composable
private fun Notice(message: String) {
	Card(
		modifier = Modifier.fillMaxWidth(),
		colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.errorContainer),
		shape = RoundedCornerShape(16.dp),
	) {
		Text(message, modifier = Modifier.padding(16.dp), color = MaterialTheme.colorScheme.onErrorContainer)
	}
}

/** Whether Bluetooth is on, kept up to date while the screen's showing. */
@Composable
private fun rememberBluetoothOn(): Boolean {
	val context = LocalContext.current
	var isOn by remember { mutableStateOf(bluetoothAdapter(context)?.isEnabled == true) }
	DisposableEffect(context) {
		val receiver = object : BroadcastReceiver() {
			override fun onReceive(context: Context, intent: Intent) {
				isOn =
					intent.getIntExtra(BluetoothAdapter.EXTRA_STATE, BluetoothAdapter.STATE_OFF) ==
					BluetoothAdapter.STATE_ON
			}
		}
		ContextCompat.registerReceiver(
			context,
			receiver,
			IntentFilter(BluetoothAdapter.ACTION_STATE_CHANGED),
			ContextCompat.RECEIVER_NOT_EXPORTED,
		)
		onDispose { context.unregisterReceiver(receiver) }
	}
	return isOn
}
