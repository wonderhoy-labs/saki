//! Gives the Android app's server mode the same NAT the command-line server uses.
//!
//! The app is Kotlin, and so is its Bluetooth and its half of the protocol. A userspace TCP/IP
//! stack is the one piece that would be a bad idea to write twice, so only that crosses over.
//! The matching declarations are in `NatEngine.kt`.

#![allow(non_snake_case)]

use bytes::Bytes;
use jni_sys::{JNI_FALSE, JNIEnv, jboolean, jbyteArray, jclass, jint, jlong};
use saki_core::nat::{Nat, NatConfig, NatInput};
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

const WORKER_THREADS: usize = 2;

struct Engine {
	runtime: Runtime,
	nat: Mutex<Option<Nat>>,
	input: NatInput,
	output: Mutex<mpsc::Receiver<Bytes>>,
}

impl Engine {
	fn start(upstream_dns: Ipv4Addr, block_lan: bool) -> std::io::Result<Self> {
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(WORKER_THREADS)
			.thread_name("saki-nat")
			.enable_all()
			.build()?;
		let mut config = NatConfig::new(upstream_dns);
		config.block_lan = block_lan;
		let mut nat = runtime.block_on(async { Nat::start(Arc::new(config)) });
		let input = nat.input();
		let output = nat.take_output().expect("a new Nat still has its output");
		Ok(Engine { runtime, nat: Mutex::new(Some(nat)), input, output: Mutex::new(output) })
	}
}

/// Starts a NAT, returning a handle for the other calls, or 0 if it couldn't start.
/// `upstream_dns` is an IPv4 address in network byte order, as `ByteBuffer.getInt()` reads it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_wonderhoylabs_saki_server_NatEngine_nativeStart(
	_env: *mut JNIEnv,
	_class: jclass,
	upstream_dns: jint,
	block_lan: jboolean,
) -> jlong {
	let upstream_dns = Ipv4Addr::from(upstream_dns as u32);
	match Engine::start(upstream_dns, block_lan != JNI_FALSE) {
		Ok(engine) => Box::into_raw(Box::new(engine)) as jlong,
		Err(_) => 0,
	}
}

/// Hands the NAT an IP packet from the client. It's dropped if the NAT has fallen behind.
///
/// # Safety
/// The handle has to come from `nativeStart` and not have been freed, and `buffer` must hold at
/// least `length` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_com_wonderhoylabs_saki_server_NatEngine_nativeInput(
	env: *mut JNIEnv,
	_class: jclass,
	handle: jlong,
	buffer: jbyteArray,
	length: jint,
) {
	let engine = unsafe { &*(handle as *const Engine) };
	let mut packet = vec![0u8; length.max(0) as usize];
	unsafe {
		let get_region = (**env).v1_1.GetByteArrayRegion;
		get_region(env, buffer, 0, length, packet.as_mut_ptr().cast());
	}
	engine.input.send(Bytes::from(packet));
}

/// Waits for an IP packet for the client and copies it into `buffer`. Returns its length, or -1
/// once the NAT has been stopped.
///
/// # Safety
/// The handle has to come from `nativeStart` and not have been freed.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_com_wonderhoylabs_saki_server_NatEngine_nativeRead(
	env: *mut JNIEnv,
	_class: jclass,
	handle: jlong,
	buffer: jbyteArray,
) -> jint {
	let engine = unsafe { &*(handle as *const Engine) };
	let packet = engine.output.lock().unwrap().blocking_recv();
	let Some(packet) = packet else {
		return -1;
	};
	unsafe {
		let capacity = ((**env).v1_1.GetArrayLength)(env, buffer);
		let length = (packet.len() as jint).min(capacity);
		let set_region = (**env).v1_1.SetByteArrayRegion;
		set_region(env, buffer, 0, length, packet.as_ptr().cast());
		length
	}
}

/// Stops the NAT and ends its connections. A thread waiting in `nativeRead` gets -1.
///
/// # Safety
/// The handle has to come from `nativeStart` and not have been freed.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_com_wonderhoylabs_saki_server_NatEngine_nativeStop(
	_env: *mut JNIEnv,
	_class: jclass,
	handle: jlong,
) {
	let engine = unsafe { &*(handle as *const Engine) };
	let _guard = engine.runtime.enter();
	engine.nat.lock().unwrap().take();
}

/// Frees a stopped NAT. Nothing may be using the handle, or use it afterwards.
///
/// # Safety
/// The handle has to come from `nativeStart`, have been stopped, and not have been freed.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn Java_com_wonderhoylabs_saki_server_NatEngine_nativeFree(
	_env: *mut JNIEnv,
	_class: jclass,
	handle: jlong,
) {
	drop(unsafe { Box::from_raw(handle as *mut Engine) });
}
