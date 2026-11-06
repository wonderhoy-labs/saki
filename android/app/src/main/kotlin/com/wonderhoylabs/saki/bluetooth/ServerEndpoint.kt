package com.wonderhoylabs.saki.bluetooth

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothGattServer
import android.bluetooth.BluetoothGattServerCallback
import android.bluetooth.BluetoothGattService
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.bluetooth.BluetoothServerSocket
import android.bluetooth.BluetoothSocket
import android.bluetooth.le.AdvertiseCallback
import android.bluetooth.le.AdvertiseData
import android.bluetooth.le.AdvertiseSettings
import android.content.Context
import android.util.Log
import com.wonderhoylabs.saki.link.Proto
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.util.concurrent.ConcurrentHashMap
import kotlin.concurrent.thread

/** A client that opened a channel to this phone. */
class IncomingLink(private val socket: BluetoothSocket, override val peerName: String) : Link {
    override val input: InputStream get() = socket.inputStream
    override val output: OutputStream get() = socket.outputStream

    override fun close() {
        runCatching { socket.close() }
    }
}

/**
 * The Bluetooth side of a server: it advertises, offers the info characteristic that tells clients
 * which channel to open, and accepts those channels.
 */
@SuppressLint("MissingPermission") // Callers check first, and Android throws if they got it wrong.
class ServerEndpoint(
    private val context: Context,
    private val onReady: (name: String) -> Unit,
    private val onClient: (Link) -> Unit,
    private val onFailed: (String) -> Unit,
) {
    private val manager = context.getSystemService(BluetoothManager::class.java)
    private val adapter = manager?.adapter
    private var listener: BluetoothServerSocket? = null
    private var gattServer: BluetoothGattServer? = null
    private var infoValue = ByteArray(0)
    /** Our own GATT client connections back to each client, which only exist to speed them up. */
    private val tuners = ConcurrentHashMap<String, BluetoothGatt>()
    @Volatile private var stopped = false
    private var triedWithoutName = false

    fun start() {
        val adapter = adapter ?: return onFailed("This phone has no Bluetooth.")
        try {
            val listener = adapter.listenUsingL2capChannel()
            this.listener = listener
            infoValue = Proto.encodeInfo(listener.psm)
            thread(name = "saki-accept", isDaemon = true) { acceptLoop(listener) }
        } catch (error: IOException) {
            return onFailed("Couldn't open an L2CAP channel: ${error.message}")
        }

        val server = manager?.openGattServer(context, gattCallback)
            ?: return onFailed("Couldn't start the GATT service.")
        gattServer = server
        val service = BluetoothGattService(SERVICE_UUID, BluetoothGattService.SERVICE_TYPE_PRIMARY)
        service.addCharacteristic(
            // Reading this needs an encrypted link, so it's what makes clients pair.
            BluetoothGattCharacteristic(
                INFO_UUID,
                BluetoothGattCharacteristic.PROPERTY_READ,
                BluetoothGattCharacteristic.PERMISSION_READ_ENCRYPTED,
            ),
        )
        // Advertising starts in onServiceAdded, once clients could actually use the service.
        server.addService(service)
    }

    fun stop() {
        stopped = true
        runCatching { adapter?.bluetoothLeAdvertiser?.stopAdvertising(advertiseCallback) }
        runCatching { listener?.close() }
        tuners.values.forEach { gatt -> runCatching { gatt.close() } }
        tuners.clear()
        runCatching { gattServer?.close() }
    }

    private fun acceptLoop(listener: BluetoothServerSocket) {
        while (!stopped) {
            val socket = try {
                listener.accept()
            } catch (error: IOException) {
                if (!stopped) onFailed("Stopped accepting clients: ${error.message}")
                return
            }
            val device = socket.remoteDevice
            onClient(IncomingLink(socket, device.name ?: device.address))
        }
    }

    private fun startAdvertising(includeName: Boolean) {
        val advertiser = adapter?.bluetoothLeAdvertiser
            ?: return onFailed("This phone can't advertise over Bluetooth Low Energy.")
        val settings = AdvertiseSettings.Builder()
            .setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY)
            .setTxPowerLevel(AdvertiseSettings.ADVERTISE_TX_POWER_HIGH)
            .setConnectable(true)
            .build()
        val data = AdvertiseData.Builder().addServiceUuid(SERVICE_PARCEL_UUID).setIncludeDeviceName(false).build()
        val response = AdvertiseData.Builder().setIncludeDeviceName(includeName).build()
        advertiser.startAdvertising(settings, data, response, advertiseCallback)
    }

    private val advertiseCallback = object : AdvertiseCallback() {
        override fun onStartSuccess(settingsInEffect: AdvertiseSettings) {
            onReady(adapter?.name ?: "this phone")
        }

        override fun onStartFailure(errorCode: Int) {
            // A long device name can be too big to fit in an advertisement.
            if (errorCode == ADVERTISE_FAILED_DATA_TOO_LARGE && !triedWithoutName) {
                triedWithoutName = true
                startAdvertising(includeName = false)
            } else {
                onFailed("Couldn't start advertising (error $errorCode).")
            }
        }
    }

    private val gattCallback = object : BluetoothGattServerCallback() {
        override fun onServiceAdded(status: Int, service: BluetoothGattService) {
            if (status == BluetoothGatt.GATT_SUCCESS) {
                startAdvertising(includeName = true)
            } else {
                onFailed("Couldn't add the GATT service (error $status).")
            }
        }

        override fun onCharacteristicReadRequest(
            device: BluetoothDevice,
            requestId: Int,
            offset: Int,
            characteristic: BluetoothGattCharacteristic,
        ) {
            val server = gattServer ?: return
            if (characteristic.uuid != INFO_UUID || offset > infoValue.size) {
                server.sendResponse(device, requestId, BluetoothGatt.GATT_INVALID_OFFSET, offset, null)
                return
            }
            val value = infoValue.copyOfRange(offset, infoValue.size)
            server.sendResponse(device, requestId, BluetoothGatt.GATT_SUCCESS, offset, value)
        }

        override fun onConnectionStateChange(device: BluetoothDevice, status: Int, newState: Int) {
            if (newState == BluetoothProfile.STATE_CONNECTED) {
                tune(device)
            } else if (newState == BluetoothProfile.STATE_DISCONNECTED) {
                tuners.remove(device.address)?.let { gatt -> runCatching { gatt.close() } }
            }
        }
    }

    /**
     * Android only lets the GATT client side ask for a faster connection, so this opens a client
     * connection over the same radio link just to ask. Clients that ask for it themselves, like
     * other phones, don't need this, but a computer's Bluetooth stack defaults to slow.
     */
    private fun tune(device: BluetoothDevice) {
        if (tuners.containsKey(device.address) || stopped) return
        // Android 17 has a newer way to call this, but Saki also runs on much older versions.
        @Suppress("DEPRECATION")
        val gatt = device.connectGatt(
            context,
            false,
            object : BluetoothGattCallback() {
                override fun onConnectionStateChange(gatt: BluetoothGatt, status: Int, newState: Int) {
                    if (newState == BluetoothProfile.STATE_CONNECTED) {
                        gatt.requestConnectionPriority(BluetoothGatt.CONNECTION_PRIORITY_HIGH)
                        gatt.setPreferredPhy(
                            BluetoothDevice.PHY_LE_2M_MASK,
                            BluetoothDevice.PHY_LE_2M_MASK,
                            BluetoothDevice.PHY_OPTION_NO_PREFERRED,
                        )
                    }
                }
            },
            BluetoothDevice.TRANSPORT_LE,
        )
        if (gatt != null) {
            tuners[device.address] = gatt
        } else {
            Log.d(TAG, "couldn't open a tuning connection to ${device.address}")
        }
    }

    private companion object {
        const val TAG = "SakiServer"
    }
}
