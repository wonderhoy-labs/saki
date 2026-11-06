package com.wonderhoylabs.saki.bluetooth

import android.Manifest
import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.ParcelUuid
import com.wonderhoylabs.saki.link.Proto
import java.io.Closeable
import java.io.InputStream
import java.io.OutputStream
import java.util.UUID

val SERVICE_UUID: UUID = UUID.fromString(Proto.SERVICE_UUID)
val SERVICE_PARCEL_UUID = ParcelUuid(SERVICE_UUID)
val INFO_UUID: UUID = UUID.fromString(Proto.INFO_UUID)

/** A byte stream to a peer, over Bluetooth or the development TCP link. */
interface Link : Closeable {
    val input: InputStream
    val output: OutputStream
    val peerName: String
}

fun bluetoothAdapter(context: Context): BluetoothAdapter? =
    context.getSystemService(BluetoothManager::class.java)?.adapter

/** The runtime permissions this app needs before it can touch Bluetooth. */
fun bluetoothPermissions(): Array<String> = buildList {
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
        add(Manifest.permission.BLUETOOTH_SCAN)
        add(Manifest.permission.BLUETOOTH_CONNECT)
        add(Manifest.permission.BLUETOOTH_ADVERTISE)
    } else {
        add(Manifest.permission.ACCESS_FINE_LOCATION)
    }
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
        add(Manifest.permission.POST_NOTIFICATIONS)
    }
    if (Build.VERSION.SDK_INT >= LOCAL_NETWORK_PERMISSION_SDK) {
        add(Manifest.permission.ACCESS_LOCAL_NETWORK)
    }
}.toTypedArray()

/** Only Bluetooth is a must. Saki can run without notifications or local network access. */
fun hasBluetoothPermissions(context: Context): Boolean = bluetoothPermissions()
    .filter { it != Manifest.permission.POST_NOTIFICATIONS && it != Manifest.permission.ACCESS_LOCAL_NETWORK }
    .all { context.checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }

/** The version of Android that started enforcing this permission. */
private const val LOCAL_NETWORK_PERMISSION_SDK = 37
