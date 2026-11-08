package com.wonderhoylabs.saki.bluetooth

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanFilter
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context

/** A Saki server found nearby. */
data class FoundServer(val device: BluetoothDevice, val name: String, val rssi: Int) {
	val address: String get() = device.address
}

/** Looks for Saki servers, reporting each sighting until it's stopped. */
@SuppressLint("MissingPermission") // Callers check first, and Android throws if they got it wrong.
class ServerScanner(
	context: Context,
	private val onFound: (FoundServer) -> Unit,
	private val onError: (Int) -> Unit = {},
) {
	private val scanner = bluetoothAdapter(context)?.bluetoothLeScanner

	private val callback = object : ScanCallback() {
		override fun onScanResult(callbackType: Int, result: ScanResult) {
			val name = result.scanRecord?.deviceName ?: result.device.name ?: result.device.address
			onFound(FoundServer(result.device, name, result.rssi))
		}

		override fun onScanFailed(errorCode: Int) {
			onError(errorCode)
		}
	}

	fun start() {
		val filter = ScanFilter.Builder().setServiceUuid(SERVICE_PARCEL_UUID).build()
		val settings = ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY).build()
		scanner?.startScan(listOf(filter), settings, callback)
	}

	fun stop() {
		// This throws if Bluetooth has been turned off in the meantime.
		runCatching { scanner?.stopScan(callback) }
	}
}
