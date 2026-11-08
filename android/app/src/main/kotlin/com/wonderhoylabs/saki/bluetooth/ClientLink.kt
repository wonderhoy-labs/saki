package com.wonderhoylabs.saki.bluetooth

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothGattCharacteristic
import android.bluetooth.BluetoothProfile
import android.bluetooth.BluetoothSocket
import android.content.Context
import android.util.Log
import com.wonderhoylabs.saki.link.Proto
import java.io.IOException
import java.io.InputStream
import java.io.OutputStream
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ExecutionException
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/**
 * Our side of a link to a server: a GATT connection, kept open because it holds the radio link and
 * its speed settings, and the L2CAP channel that carries the actual traffic.
 */
@SuppressLint("MissingPermission") // Callers check first, and Android throws if they got it wrong.
class ClientLink private constructor(
	private val gatt: BluetoothGatt,
	private val socket: BluetoothSocket,
	override val peerName: String,
) : Link {
	override val input: InputStream get() = socket.inputStream
	override val output: OutputStream get() = socket.outputStream

	override fun close() {
		runCatching { socket.close() }
		runCatching { gatt.disconnect() }
		runCatching { gatt.close() }
	}

	companion object {
		private const val TAG = "SakiLink"

		/** Long enough for someone to answer the pairing dialog. */
		private const val OPEN_TIMEOUT_SECONDS = 90L

		private fun phyName(phy: Int) = when (phy) {
			BluetoothDevice.PHY_LE_1M -> "1M"
			BluetoothDevice.PHY_LE_2M -> "2M"
			BluetoothDevice.PHY_LE_CODED -> "coded"
			else -> "unknown ($phy)"
		}

		/**
		 * Connects, pairing first if needed. Android starts pairing by itself when the read of the
		 * info characteristic comes back asking for encryption.
		 */
		fun open(context: Context, device: BluetoothDevice, name: String): ClientLink {
			val psmResult = CompletableFuture<Int>()
			val callback = object : BluetoothGattCallback() {
				override fun onConnectionStateChange(gatt: BluetoothGatt, status: Int, newState: Int) {
					when {
						newState == BluetoothProfile.STATE_CONNECTED -> {
							gatt.requestConnectionPriority(BluetoothGatt.CONNECTION_PRIORITY_HIGH)
							gatt.discoverServices()
						}

						newState == BluetoothProfile.STATE_DISCONNECTED -> {
							psmResult.completeExceptionally(IOException("Couldn't connect to $name (error $status)."))
						}
					}
				}

				override fun onServicesDiscovered(gatt: BluetoothGatt, status: Int) {
					val characteristic = gatt.getService(SERVICE_UUID)?.getCharacteristic(INFO_UUID)
					if (status != BluetoothGatt.GATT_SUCCESS || characteristic == null) {
						psmResult.completeExceptionally(IOException("$name doesn't offer the Saki service."))
						return
					}
					if (!gatt.readCharacteristic(characteristic)) {
						psmResult.completeExceptionally(IOException("Couldn't read $name's info."))
					}
				}

				override fun onCharacteristicRead(
					gatt: BluetoothGatt,
					characteristic: BluetoothGattCharacteristic,
					value: ByteArray,
					status: Int,
				) {
					handleInfo(value, status)
				}

				@Deprecated("Android 12 and older call this version instead.")
				override fun onCharacteristicRead(gatt: BluetoothGatt, characteristic: BluetoothGattCharacteristic, status: Int) {
					@Suppress("DEPRECATION")
					handleInfo(characteristic.value ?: ByteArray(0), status)
				}

				override fun onPhyUpdate(gatt: BluetoothGatt, txPhy: Int, rxPhy: Int, status: Int) {
					Log.i(TAG, "radio mode: tx ${phyName(txPhy)}, rx ${phyName(rxPhy)}")
				}

				override fun onPhyRead(gatt: BluetoothGatt, txPhy: Int, rxPhy: Int, status: Int) {
					Log.i(TAG, "radio mode: tx ${phyName(txPhy)}, rx ${phyName(rxPhy)}")
				}

				private fun handleInfo(value: ByteArray, status: Int) {
					if (status != BluetoothGatt.GATT_SUCCESS) {
						psmResult.completeExceptionally(
							IOException(
								"Couldn't pair with $name. If it's a Mac, it can't accept a first-time pairing as a server. " +
									"Share this phone's network and run saki probe on the Mac once, then try again.",
							),
						)
						return
					}
					try {
						val (version, psm) = Proto.decodeInfo(value)
						if (version != Proto.VERSION) {
							throw IOException(
								"$name speaks protocol $version, and this is ${Proto.VERSION}. Update the older one.",
							)
						}
						psmResult.complete(psm)
					} catch (error: IOException) {
						psmResult.completeExceptionally(error)
					}
				}
			}

			// Android 17 has a newer way to call this, but Saki also runs on much older versions.
			@Suppress("DEPRECATION")
			val gatt = device.connectGatt(context, false, callback, BluetoothDevice.TRANSPORT_LE)
				?: throw IOException("Couldn't start connecting to $name.")
			try {
				val psm = try {
					psmResult.get(OPEN_TIMEOUT_SECONDS, TimeUnit.SECONDS)
				} catch (_: TimeoutException) {
					throw IOException("Connecting to $name timed out. Did the pairing prompt get answered?")
				} catch (error: ExecutionException) {
					throw error.cause as? IOException ?: IOException(error.cause)
				}
				gatt.setPreferredPhy(
					BluetoothDevice.PHY_LE_2M_MASK,
					BluetoothDevice.PHY_LE_2M_MASK,
					BluetoothDevice.PHY_OPTION_NO_PREFERRED,
				)
				gatt.readPhy()
				val socket = device.createL2capChannel(psm)
				try {
					socket.connect()
				} catch (error: IOException) {
					runCatching { socket.close() }
					throw IOException("Couldn't open the channel to $name: ${error.message}", error)
				}
				// Discovering services puts the connection's settings back to the default, which wastes
				// most of the link. A Mac asks for a fast connection by itself, but Linux doesn't.
				gatt.requestConnectionPriority(BluetoothGatt.CONNECTION_PRIORITY_HIGH)
				Log.i(
					TAG,
					"channel open: sends up to ${socket.maxTransmitPacketSize}, receives up to ${socket.maxReceivePacketSize}",
				)
				return ClientLink(gatt, socket, name)
			} catch (error: Throwable) {
				runCatching { gatt.disconnect() }
				runCatching { gatt.close() }
				throw error
			}
		}
	}
}
