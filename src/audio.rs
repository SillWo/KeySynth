use std::mem::size_of;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
#[cfg(test)]
use std::sync::atomic::{AtomicU32, AtomicU64};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, HWND, LPARAM, WAIT_FAILED, WAIT_OBJECT_0, WPARAM,
};
use windows::Win32::Media::Audio::{
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
    AUDCLNT_STREAMFLAGS_EVENTCALLBACK, AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, IAudioClient,
    IAudioClient3, IAudioRenderClient, IMMDeviceEnumerator, MMDeviceEnumerator, WAVE_FORMAT_PCM,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, eConsole, eRender,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    CoUninitialize,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForMultipleObjects};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::GUID;

#[cfg(test)]
use crate::events::KeyEvent;
use crate::events::Shared;
use crate::keyboard;
use crate::synth::Synth;

const WM_AUDIO_ERROR: u32 = 0x8000 + 2;
const WM_AUDIO_READY: u32 = 0x8000 + 3;
const RECONNECT_START_MS: u64 = 250;
const RECONNECT_MAX_MS: u64 = 5_000;
const WAVE_FORMAT_IEEE_FLOAT: u32 = 3;

pub struct AudioEngine {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    wake_event: Option<OwnedHandle>,
    #[cfg(test)]
    diagnostics: Arc<AudioDiagnostics>,
}

impl AudioEngine {
    pub fn start(shared: Arc<Shared>) -> Result<Self, String> {
        let input_event = OwnedHandle::event()?;
        let handle_value = input_event.0.0 as isize;
        if shared
            .wake_handle
            .compare_exchange(0, handle_value, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err("The audio worker is already running.".to_owned());
        }

        let worker_shared = shared.clone();
        let input_event_raw = handle_value;
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        #[cfg(test)]
        let diagnostics = Arc::new(AudioDiagnostics::new());
        #[cfg(test)]
        let worker_diagnostics = diagnostics.clone();
        let worker = match thread::Builder::new()
            .name("keysynth-audio".to_owned())
            .spawn(move || {
                audio_thread(
                    worker_shared,
                    input_event_raw,
                    ready_tx,
                    #[cfg(test)]
                    worker_diagnostics,
                );
            }) {
            Ok(worker) => worker,
            Err(error) => {
                shared.wake_handle.store(0, Ordering::Release);
                return Err(format!("Could not start the audio worker: {error}"));
            }
        };

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                shared,
                worker: Some(worker),
                wake_event: Some(input_event),
                #[cfg(test)]
                diagnostics,
            }),
            Ok(Err(error)) => {
                shared.shutdown.store(true, Ordering::Release);
                keyboard::notify_audio(&shared);
                let _ = worker.join();
                Err(error)
            }
            Err(_) => {
                shared.shutdown.store(true, Ordering::Release);
                keyboard::notify_audio(&shared);
                let _ = worker.join();
                Err("The audio worker stopped during startup.".to_owned())
            }
        }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        keyboard::notify_audio(&self.shared);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.shared.wake_handle.store(0, Ordering::Release);
        drop(self.wake_event.take());
    }
}

#[cfg(test)]
struct AudioDiagnostics {
    frames: AtomicU64,
    peak_bits: AtomicU32,
    backend: AtomicU32,
    sample_rate: AtomicU32,
    buffer_frames: AtomicU32,
    period_frames: AtomicU32,
    errors: AtomicU32,
    event_sent_us: AtomicU64,
    first_buffer_latency_us: AtomicU64,
    first_start_latency_us: AtomicU64,
    restart_start_latency_us: AtomicU64,
    start_count: AtomicU32,
    started_at: Instant,
}

#[cfg(test)]
impl AudioDiagnostics {
    fn new() -> Self {
        Self {
            frames: AtomicU64::new(0),
            peak_bits: AtomicU32::new(0),
            backend: AtomicU32::new(0),
            sample_rate: AtomicU32::new(0),
            buffer_frames: AtomicU32::new(0),
            period_frames: AtomicU32::new(0),
            errors: AtomicU32::new(0),
            event_sent_us: AtomicU64::new(0),
            first_buffer_latency_us: AtomicU64::new(0),
            first_start_latency_us: AtomicU64::new(0),
            restart_start_latency_us: AtomicU64::new(0),
            start_count: AtomicU32::new(0),
            started_at: Instant::now(),
        }
    }
}

struct ComApartment;

impl ComApartment {
    fn initialize() -> Result<Self, String> {
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            Ok(Self)
        } else {
            Err(format!("Could not initialize COM: {hr}"))
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

struct OwnedHandle(HANDLE);

impl OwnedHandle {
    fn event() -> Result<Self, String> {
        unsafe { CreateEventW(None, false, false, None) }
            .map(Self)
            .map_err(|error| format!("Could not create a WASAPI event: {error}"))
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct OwnedWaveFormat(*mut WAVEFORMATEX);

impl Drop for OwnedWaveFormat {
    fn drop(&mut self) {
        unsafe {
            CoTaskMemFree(Some(self.0.cast()));
        }
    }
}

enum AudioClient {
    LowLatency(IAudioClient3),
    Shared(IAudioClient),
}

impl AudioClient {
    fn render_client(&self) -> windows::core::Result<IAudioRenderClient> {
        unsafe {
            match self {
                Self::LowLatency(client) => client.GetService(),
                Self::Shared(client) => client.GetService(),
            }
        }
    }
    fn buffer_size(&self) -> windows::core::Result<u32> {
        unsafe {
            match self {
                Self::LowLatency(client) => client.GetBufferSize(),
                Self::Shared(client) => client.GetBufferSize(),
            }
        }
    }
    fn padding(&self) -> windows::core::Result<u32> {
        unsafe {
            match self {
                Self::LowLatency(client) => client.GetCurrentPadding(),
                Self::Shared(client) => client.GetCurrentPadding(),
            }
        }
    }
    fn set_event(&self, event: HANDLE) -> windows::core::Result<()> {
        unsafe {
            match self {
                Self::LowLatency(client) => client.SetEventHandle(event),
                Self::Shared(client) => client.SetEventHandle(event),
            }
        }
    }
    fn start(&self) -> windows::core::Result<()> {
        unsafe {
            match self {
                Self::LowLatency(client) => client.Start(),
                Self::Shared(client) => client.Start(),
            }
        }
    }
    fn stop_reset(&self) {
        unsafe {
            match self {
                Self::LowLatency(client) => {
                    let _ = client.Stop();
                    let _ = client.Reset();
                }
                Self::Shared(client) => {
                    let _ = client.Stop();
                    let _ = client.Reset();
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum SampleFormat {
    Float32,
    Pcm16,
    Pcm24,
    Pcm32,
}

#[derive(Clone, Copy)]
struct MixFormat {
    channels: usize,
    block_align: usize,
    bytes_per_sample: usize,
    sample_rate: u32,
    format: SampleFormat,
}

struct Device {
    client: AudioClient,
    render: IAudioRenderClient,
    event: OwnedHandle,
    format: MixFormat,
    buffer_frames: u32,
    #[cfg(test)]
    backend: u32,
    #[cfg(test)]
    period_frames: u32,
}

impl Device {
    fn open() -> Result<Self, String> {
        unsafe {
            let enumerator: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|error| {
                    format!("Could not find the Windows audio service: {error}")
                })?;
            let endpoint = enumerator
                .GetDefaultAudioEndpoint(eRender, eConsole)
                .map_err(|error| format!("Could not open the default output device: {error}"))?;

            let low_latency = endpoint.Activate::<IAudioClient3>(CLSCTX_ALL, None);
            if let Ok(client) = low_latency
                && let Ok(mix_ptr) = client.GetMixFormat()
            {
                let mix_ptr = OwnedWaveFormat(mix_ptr);
                let Ok(format) = read_mix_format(mix_ptr.0) else {
                    return Self::open_shared(&endpoint);
                };
                let mut default_frames = 0;
                let mut fundamental = 0;
                let mut minimum = 0;
                let mut maximum = 0;
                let engine_ok = client
                    .GetSharedModeEnginePeriod(
                        mix_ptr.0,
                        &mut default_frames,
                        &mut fundamental,
                        &mut minimum,
                        &mut maximum,
                    )
                    .is_ok();
                let desired = ((format.sample_rate as f64 * 0.005).round() as u32).max(1);
                let mut period = if engine_ok && fundamental != 0 {
                    let base = desired.max(minimum).min(maximum);
                    let steps = base.saturating_sub(minimum).div_ceil(fundamental);
                    (minimum.saturating_add(steps.saturating_mul(fundamental)))
                        .min(maximum)
                        .max(minimum)
                } else {
                    default_frames.max(1)
                };
                if period == 0 {
                    period = desired;
                }
                let Ok(event) = OwnedHandle::event() else {
                    return Self::open_shared(&endpoint);
                };
                let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK;
                let result = client.InitializeSharedAudioStream(flags, period, mix_ptr.0, None);
                if result.is_ok() {
                    let client = AudioClient::LowLatency(client);
                    return Self::finish(client, event, format, 1, period);
                }
            }

            // Windows versions/devices without AudioClient3 use ordinary shared
            // event mode. WASAPI's shared mix format is accepted without a
            // custom conversion and still gives the engine the requested format.
            Self::open_shared(&endpoint)
        }
    }

    unsafe fn open_shared(
        endpoint: &windows::Win32::Media::Audio::IMMDevice,
    ) -> Result<Self, String> {
        unsafe {
            let client: IAudioClient = endpoint
                .Activate(CLSCTX_ALL, None)
                .map_err(|error| format!("Could not activate the default audio device: {error}"))?;
            let mix_ptr = client
                .GetMixFormat()
                .map_err(|error| format!("Could not read the output device format: {error}"))?;
            let mix_ptr = OwnedWaveFormat(mix_ptr);
            let format = read_mix_format(mix_ptr.0)?;
            let event = OwnedHandle::event()?;
            let flags = AUDCLNT_STREAMFLAGS_EVENTCALLBACK
                | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
                | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
            let init = client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 0, 0, mix_ptr.0, None);
            init.map_err(|error| format!("Could not initialize Windows audio output: {error}"))?;
            Self::finish(AudioClient::Shared(client), event, format, 2, 0)
        }
    }

    unsafe fn finish(
        client: AudioClient,
        event: OwnedHandle,
        format: MixFormat,
        backend: u32,
        period_frames: u32,
    ) -> Result<Self, String> {
        client
            .set_event(event.0)
            .map_err(|error| format!("Could not connect the WASAPI render event: {error}"))?;
        let render = client
            .render_client()
            .map_err(|error| format!("Could not open the WASAPI render buffer: {error}"))?;
        let buffer_frames = client
            .buffer_size()
            .map_err(|error| format!("Could not read the WASAPI buffer size: {error}"))?;
        if buffer_frames == 0 {
            return Err("The output device returned an empty audio buffer.".to_owned());
        }
        #[cfg(not(test))]
        let _ = (backend, period_frames);
        let device = Self {
            client,
            render,
            event,
            format,
            buffer_frames,
            #[cfg(test)]
            backend,
            #[cfg(test)]
            period_frames: if period_frames == 0 {
                buffer_frames
            } else {
                period_frames
            },
        };
        device.warm_up()?;
        Ok(device)
    }

    fn warm_up(&self) -> Result<(), String> {
        let data = unsafe { self.render.GetBuffer(self.buffer_frames) }.map_err(|error| {
            format!("Could not acquire a silent WASAPI warm-up buffer: {error}")
        })?;
        // SAFETY: WASAPI granted the complete render buffer. Submitting it with
        // the silent flag makes warm-up inaudible and avoids format conversion.
        let _ = data;
        if let Err(error) = unsafe {
            self.render
                .ReleaseBuffer(self.buffer_frames, AUDCLNT_BUFFERFLAGS_SILENT.0 as u32)
        } {
            self.client.stop_reset();
            return Err(format!(
                "Could not submit the silent WASAPI warm-up buffer: {error}"
            ));
        }
        if let Err(error) = self.client.start() {
            self.client.stop_reset();
            return Err(format!("Could not warm up Windows audio output: {error}"));
        }
        let stop = match &self.client {
            AudioClient::LowLatency(client) => unsafe { client.Stop() },
            AudioClient::Shared(client) => unsafe { client.Stop() },
        };
        if let Err(error) = stop {
            self.client.stop_reset();
            return Err(format!(
                "Could not stop Windows audio after warm-up: {error}"
            ));
        }
        let reset = match &self.client {
            AudioClient::LowLatency(client) => unsafe { client.Reset() },
            AudioClient::Shared(client) => unsafe { client.Reset() },
        };
        reset.map_err(|error| format!("Could not reset Windows audio after warm-up: {error}"))
    }

    fn start(&self, synth: &mut Synth, diagnostics: &DiagnosticsArg) -> Result<(), String> {
        // Fill the complete buffer before starting, so a just-pressed key does
        // not wait for an extra device period before becoming audible.
        self.fill(synth, self.buffer_frames, diagnostics)?;
        self.client
            .start()
            .map_err(|error| format!("Could not start Windows audio output: {error}"))?;
        #[cfg(test)]
        if let Some(diagnostics) = diagnostics {
            let event_sent_us = diagnostics.event_sent_us.load(Ordering::Relaxed);
            if event_sent_us != 0 {
                let elapsed_us = diagnostics.started_at.elapsed().as_micros() as u64;
                let latency_us = elapsed_us.saturating_sub(event_sent_us).max(1);
                match diagnostics.start_count.fetch_add(1, Ordering::Relaxed) {
                    0 => diagnostics
                        .first_start_latency_us
                        .store(latency_us, Ordering::Relaxed),
                    1 => diagnostics
                        .restart_start_latency_us
                        .store(latency_us, Ordering::Relaxed),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn render_available(
        &self,
        synth: &mut Synth,
        diagnostics: &DiagnosticsArg,
    ) -> Result<(), String> {
        let padding = self
            .client
            .padding()
            .map_err(|error| format!("Could not query Windows audio padding: {error}"))?;
        let frames = self.buffer_frames.saturating_sub(padding);
        if frames > 0 {
            self.fill(synth, frames, diagnostics)?;
        }
        Ok(())
    }

    fn fill(
        &self,
        synth: &mut Synth,
        frames: u32,
        diagnostics: &DiagnosticsArg,
    ) -> Result<(), String> {
        let data = unsafe { self.render.GetBuffer(frames) }
            .map_err(|error| format!("Could not acquire the Windows audio buffer: {error}"))?;
        let mut peak = 0.0f32;
        for frame in 0..frames as usize {
            let sample = synth.next_sample();
            peak = peak.max(sample.abs());
            if self.format.block_align > self.format.channels * self.format.bytes_per_sample {
                // Zero any non-audio alignment bytes before writing channels.
                unsafe {
                    ptr::write_bytes(
                        data.add(frame * self.format.block_align),
                        0,
                        self.format.block_align,
                    );
                }
            }
            for channel in 0..self.format.channels {
                let offset =
                    frame * self.format.block_align + channel * self.format.bytes_per_sample;
                // SAFETY: WASAPI returned `frames` writable frames and the
                // accepted mix format's block alignment was validated.
                unsafe {
                    write_sample(data.add(offset), sample, self.format.format);
                }
            }
        }
        unsafe { self.render.ReleaseBuffer(frames, 0) }
            .map_err(|error| format!("Could not submit the Windows audio buffer: {error}"))?;
        #[cfg(test)]
        if let Some(diagnostics) = diagnostics {
            let event_sent_us = diagnostics.event_sent_us.load(Ordering::Relaxed);
            if event_sent_us != 0
                && diagnostics.first_buffer_latency_us.load(Ordering::Relaxed) == 0
            {
                let elapsed_us = diagnostics.started_at.elapsed().as_micros() as u64;
                let latency_us = elapsed_us.saturating_sub(event_sent_us).max(1);
                let _ = diagnostics.first_buffer_latency_us.compare_exchange(
                    0,
                    latency_us,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                );
            }
            diagnostics
                .frames
                .fetch_add(frames as u64, Ordering::Relaxed);
            let value = peak.to_bits();
            let mut current = diagnostics.peak_bits.load(Ordering::Relaxed);
            while f32::from_bits(current) < peak {
                match diagnostics.peak_bits.compare_exchange_weak(
                    current,
                    value,
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                ) {
                    Ok(_) => break,
                    Err(actual) => current = actual,
                }
            }
        }
        #[cfg(not(test))]
        let _ = (diagnostics, peak);
        Ok(())
    }

    fn stop_and_reset(&self) {
        self.client.stop_reset();
    }
}

#[cfg(test)]
type DiagnosticsArg = Option<Arc<AudioDiagnostics>>;
#[cfg(not(test))]
type DiagnosticsArg = ();

fn audio_thread(
    shared: Arc<Shared>,
    input_event_raw: isize,
    ready: mpsc::SyncSender<Result<(), String>>,
    #[cfg(test)] diagnostics: Arc<AudioDiagnostics>,
) {
    let input_event = HANDLE(input_event_raw as *mut std::ffi::c_void);
    let _com = match ComApartment::initialize() {
        Ok(com) => com,
        Err(error) => {
            post_audio_status(&shared, WM_AUDIO_ERROR);
            let _ = ready.send(Err(error));
            clear_wake_handle(&shared, input_event);
            return;
        }
    };
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64)
        ^ (std::process::id() as u64);
    let mut device: Option<Device> = match Device::open() {
        Ok(device) => {
            post_audio_status(&shared, WM_AUDIO_READY);
            Some(device)
        }
        Err(_error) => {
            #[cfg(test)]
            record_audio_error(&diagnostics, &_error);
            post_audio_status(&shared, WM_AUDIO_ERROR);
            None
        }
    };
    let mut synth = Synth::new(
        device
            .as_ref()
            .map_or(48_000, |device| device.format.sample_rate),
        seed,
    );
    let _ = ready.send(Ok(()));
    let mut playing = false;
    let mut retry_ms = RECONNECT_START_MS;
    let mut reported_error = device.is_none();
    #[cfg(test)]
    let diagnostics_arg = Some(diagnostics.clone());
    #[cfg(not(test))]
    let diagnostics_arg = ();
    #[cfg(test)]
    if let Some(device) = device.as_ref() {
        record_backend(&diagnostics, device);
    }

    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            break;
        }
        if device.is_none() {
            if !wait_retry(&shared, input_event, retry_ms) {
                break;
            }
            clear_queued_input(&shared);
            synth.reset();
            match Device::open() {
                Ok(new_device) => {
                    #[cfg(test)]
                    record_backend(&diagnostics, &new_device);
                    synth = Synth::new(new_device.format.sample_rate, seed);
                    device = Some(new_device);
                    retry_ms = RECONNECT_START_MS;
                    post_audio_status(&shared, WM_AUDIO_READY);
                    reported_error = false;
                }
                Err(_error) => {
                    #[cfg(test)]
                    record_audio_error(&diagnostics, &_error);
                    if !reported_error {
                        post_audio_status(&shared, WM_AUDIO_ERROR);
                    }
                    reported_error = true;
                    retry_ms = (retry_ms * 2).min(RECONNECT_MAX_MS);
                    continue;
                }
            }
        }
        let active_device = device.as_ref().expect("device opened above");
        if !playing {
            let wait = unsafe { WaitForMultipleObjects(&[input_event], false, u32::MAX) };
            if shared.shutdown.load(Ordering::Acquire) {
                break;
            }
            if wait == WAIT_FAILED {
                post_audio_status(&shared, WM_AUDIO_ERROR);
                device = None;
                synth.reset();
                reported_error = true;
                continue;
            }
            if wait != WAIT_OBJECT_0 {
                continue;
            }
            if drain_input(&mut synth, &shared) && synth.is_active() {
                match active_device.start(&mut synth, &diagnostics_arg) {
                    Ok(()) => playing = true,
                    Err(_error) => {
                        #[cfg(test)]
                        record_audio_error(&diagnostics, &_error.to_string());
                        active_device.stop_and_reset();
                        post_audio_status(&shared, WM_AUDIO_ERROR);
                        device = None;
                        synth.reset();
                        clear_queued_input(&shared);
                        reported_error = true;
                    }
                }
            }
            continue;
        }

        let handles = [active_device.event.0, input_event];
        let wait = unsafe { WaitForMultipleObjects(&handles, false, 1000) };
        if shared.shutdown.load(Ordering::Acquire) {
            break;
        }
        if wait == WAIT_OBJECT_0 {
            let _ = drain_input(&mut synth, &shared);
            let render_result = if synth.is_active() {
                active_device.render_available(&mut synth, &diagnostics_arg)
            } else {
                match active_device.client.padding() {
                    Ok(0) => {
                        active_device.stop_and_reset();
                        playing = false;
                        Ok(())
                    }
                    Ok(_) => Ok(()), // Let the release samples already queued reach the speakers.
                    Err(error) => Err(format!("Could not query Windows audio padding: {error}")),
                }
            };
            if let Err(_error) = render_result {
                #[cfg(test)]
                record_audio_error(&diagnostics, &_error);
                active_device.stop_and_reset();
                device = None;
                synth.reset();
                clear_queued_input(&shared);
                if !reported_error {
                    post_audio_status(&shared, WM_AUDIO_ERROR);
                }
                reported_error = true;
                retry_ms = RECONNECT_START_MS;
                playing = false;
            }
        } else if wait.0 == WAIT_OBJECT_0_PLUS_ONE {
            let _ = drain_input(&mut synth, &shared);
        } else if wait.0 == windows::Win32::Foundation::WAIT_TIMEOUT.0 {
            // A disconnected endpoint can stop signaling its event. Querying
            // padding periodically detects that loss while keeping idle CPU low.
            if let Err(_error) = active_device.client.padding() {
                #[cfg(test)]
                record_audio_error(&diagnostics, &_error.to_string());
                active_device.stop_and_reset();
                device = None;
                synth.reset();
                clear_queued_input(&shared);
                if !reported_error {
                    post_audio_status(&shared, WM_AUDIO_ERROR);
                }
                reported_error = true;
                retry_ms = RECONNECT_START_MS;
                playing = false;
            }
        } else if wait == WAIT_FAILED {
            #[cfg(test)]
            record_audio_error(
                &diagnostics,
                "WaitForMultipleObjects failed while rendering audio",
            );
            active_device.stop_and_reset();
            device = None;
            synth.reset();
            clear_queued_input(&shared);
            if !reported_error {
                post_audio_status(&shared, WM_AUDIO_ERROR);
            }
            reported_error = true;
            retry_ms = RECONNECT_START_MS;
            playing = false;
        }
    }
    if playing && let Some(device) = device.as_ref() {
        device.stop_and_reset();
    }
    clear_wake_handle(&shared, input_event);
}

const WAIT_OBJECT_0_PLUS_ONE: u32 = WAIT_OBJECT_0.0 + 1;

fn drain_input(synth: &mut Synth, shared: &Shared) -> bool {
    let overflowed = shared.overflow.swap(false, Ordering::AcqRel) || shared.queue.take_overflow();
    if overflowed {
        clear_queued_input(shared);
        synth.reset();
        synth.sync_parameters(shared);
        return false;
    }
    synth.sync_parameters(shared);
    while let Some(event) = shared.queue.pop() {
        synth.handle_event(event, shared);
    }
    synth.is_active()
}

fn wait_retry(shared: &Shared, input_event: HANDLE, delay_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(delay_ms);
    loop {
        if shared.shutdown.load(Ordering::Acquire) {
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        let wait_ms = remaining.as_millis().clamp(1, u32::MAX as u128) as u32;
        let _ = unsafe { WaitForMultipleObjects(&[input_event], false, wait_ms) };
        // Discard events while audio is unavailable and continue the backoff
        // deadline, so sustained typing cannot turn retries into a busy loop.
        clear_queued_input(shared);
    }
}

fn clear_queued_input(shared: &Shared) {
    while shared.queue.pop().is_some() {}
    shared.overflow.store(false, Ordering::Release);
    let _ = shared.queue.take_overflow();
}

#[cfg(test)]
fn record_backend(diagnostics: &AudioDiagnostics, device: &Device) {
    diagnostics.backend.store(device.backend, Ordering::Relaxed);
    diagnostics
        .sample_rate
        .store(device.format.sample_rate, Ordering::Relaxed);
    diagnostics
        .buffer_frames
        .store(device.buffer_frames, Ordering::Relaxed);
    diagnostics
        .period_frames
        .store(device.period_frames, Ordering::Relaxed);
}

#[cfg(test)]
fn record_audio_error(diagnostics: &AudioDiagnostics, error: &str) {
    diagnostics.errors.fetch_add(1, Ordering::Relaxed);
    eprintln!("WASAPI test diagnostic: {error}");
}

fn clear_wake_handle(shared: &Shared, input_event: HANDLE) {
    let _ = shared.wake_handle.compare_exchange(
        input_event.0 as isize,
        0,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}

fn post_audio_status(shared: &Shared, message: u32) {
    let hwnd = shared.window_handle.load(Ordering::Acquire);
    if hwnd != 0 {
        let _ = unsafe {
            PostMessageW(
                Some(HWND(hwnd as *mut std::ffi::c_void)),
                message,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }
}

unsafe fn read_mix_format(format: *mut WAVEFORMATEX) -> Result<MixFormat, String> {
    if format.is_null() {
        return Err("The output device returned no audio format.".to_owned());
    }
    let wave = unsafe { ptr::read_unaligned(format) };
    let channels = wave.nChannels as usize;
    let sample_rate = wave.nSamplesPerSec;
    let block_align = wave.nBlockAlign as usize;
    let (tag, bits) = if wave.wFormatTag as u32 == WAVE_FORMAT_EXTENSIBLE {
        if (wave.cbSize as usize) < size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>() {
            return Err(
                "The output device returned an incomplete extensible audio format.".to_owned(),
            );
        }
        let extended = unsafe { ptr::read_unaligned(format.cast::<WAVEFORMATEXTENSIBLE>()) };
        let subformat = unsafe { ptr::addr_of!(extended.SubFormat).read_unaligned() };
        if subformat == float_subtype() {
            (WAVE_FORMAT_IEEE_FLOAT as u16, wave.wBitsPerSample)
        } else if subformat == KSDATAFORMAT_SUBTYPE_PCM {
            (WAVE_FORMAT_PCM as u16, wave.wBitsPerSample)
        } else {
            return Err(
                "The output device uses an unsupported extensible audio format.".to_owned(),
            );
        }
    } else {
        (wave.wFormatTag, wave.wBitsPerSample)
    };
    if channels == 0 || channels > 32 || sample_rate == 0 {
        return Err("The output device returned invalid audio dimensions.".to_owned());
    }
    let (format, bytes_per_sample) = match (tag as u32, bits) {
        (WAVE_FORMAT_IEEE_FLOAT, 32) => (SampleFormat::Float32, 4),
        (WAVE_FORMAT_PCM, 16) => (SampleFormat::Pcm16, 2),
        (WAVE_FORMAT_PCM, 24) => (SampleFormat::Pcm24, 3),
        (WAVE_FORMAT_PCM, 32) => (SampleFormat::Pcm32, 4),
        _ => {
            return Err(format!(
                "The output device audio format ({tag}, {bits} bit) is unsupported."
            ));
        }
    };
    if block_align < channels * bytes_per_sample {
        return Err("The output device returned an invalid audio frame size.".to_owned());
    }
    Ok(MixFormat {
        channels,
        block_align,
        bytes_per_sample,
        sample_rate,
        format,
    })
}

fn float_subtype() -> GUID {
    GUID::from_u128(0x00000003_0000_0010_8000_00aa00389b71)
}

unsafe fn write_sample(destination: *mut u8, sample: f32, format: SampleFormat) {
    match format {
        SampleFormat::Float32 => unsafe {
            destination.cast::<f32>().write_unaligned(sample);
        },
        SampleFormat::Pcm16 => unsafe {
            destination
                .cast::<i16>()
                .write_unaligned((sample * i16::MAX as f32) as i16);
        },
        SampleFormat::Pcm24 => {
            let value = (sample * 8_388_607.0) as i32;
            unsafe {
                destination.write(value as u8);
                destination.add(1).write((value >> 8) as u8);
                destination.add(2).write((value >> 16) as u8);
            }
        }
        SampleFormat::Pcm32 => unsafe {
            destination
                .cast::<i32>()
                .write_unaligned((sample * i32::MAX as f32) as i32);
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;

    #[test]
    #[ignore = "requires a working Windows default audio output device"]
    fn wasapi_renders_audio_and_shuts_down_cleanly() {
        let shared = Arc::new(Shared::new(&Settings::default()));
        let engine = AudioEngine::start(shared.clone()).expect("audio device should start");
        engine.diagnostics.event_sent_us.store(
            engine.diagnostics.started_at.elapsed().as_micros() as u64,
            Ordering::Relaxed,
        );
        assert!(shared.queue.push(KeyEvent {
            key_id: 0x2c,
            down: true
        }));
        assert!(shared.queue.push(KeyEvent {
            key_id: 0x2d,
            down: true
        }));
        keyboard::notify_audio(&shared);
        thread::sleep(Duration::from_millis(1_000));
        let chord_frames = engine.diagnostics.frames.load(Ordering::Relaxed);
        let sample_rate = engine.diagnostics.sample_rate.load(Ordering::Relaxed);
        assert!(
            chord_frames >= (sample_rate / 2) as u64,
            "held notes rendered only {chord_frames} frames at {sample_rate} Hz"
        );
        assert!(shared.queue.push(KeyEvent {
            key_id: 0x2c,
            down: false
        }));
        assert!(shared.queue.push(KeyEvent {
            key_id: 0x2d,
            down: false
        }));
        keyboard::notify_audio(&shared);
        let release_start = engine.diagnostics.frames.load(Ordering::Relaxed);
        thread::sleep(Duration::from_millis(150));
        let release_frames = engine
            .diagnostics
            .frames
            .load(Ordering::Relaxed)
            .saturating_sub(release_start);
        thread::sleep(Duration::from_millis(5_000));
        let idle_frames = engine.diagnostics.frames.load(Ordering::Relaxed);
        assert_eq!(
            idle_frames,
            release_start + release_frames,
            "audio stream kept rendering after release drained"
        );
        engine.diagnostics.event_sent_us.store(
            engine.diagnostics.started_at.elapsed().as_micros() as u64,
            Ordering::Relaxed,
        );
        assert!(shared.queue.push(KeyEvent {
            key_id: 0x30,
            down: true
        }));
        keyboard::notify_audio(&shared);
        let restart_start = engine.diagnostics.frames.load(Ordering::Relaxed);
        thread::sleep(Duration::from_millis(150));
        let restart_frames = engine
            .diagnostics
            .frames
            .load(Ordering::Relaxed)
            .saturating_sub(restart_start);
        assert!(shared.queue.push(KeyEvent {
            key_id: 0x30,
            down: false
        }));
        keyboard::notify_audio(&shared);
        assert!(
            restart_frames >= (sample_rate / 20) as u64,
            "restarted note rendered only {restart_frames} frames at {sample_rate} Hz"
        );
        let frames = engine.diagnostics.frames.load(Ordering::Relaxed);
        let first_buffer_latency_us = engine
            .diagnostics
            .first_buffer_latency_us
            .load(Ordering::Relaxed);
        let first_start_latency_us = engine
            .diagnostics
            .first_start_latency_us
            .load(Ordering::Relaxed);
        let restart_start_latency_us = engine
            .diagnostics
            .restart_start_latency_us
            .load(Ordering::Relaxed);
        let peak = f32::from_bits(engine.diagnostics.peak_bits.load(Ordering::Relaxed));
        let backend = match engine.diagnostics.backend.load(Ordering::Relaxed) {
            1 => "IAudioClient3 shared engine period",
            2 => "ordinary shared WASAPI",
            _ => "unknown",
        };
        eprintln!(
            "WASAPI render: backend={backend}, sample_rate={sample_rate}, buffer_frames={}, period_frames={}, first_event_to_buffer_us={first_buffer_latency_us}, first_event_to_start_us={first_start_latency_us}, restart_event_to_start_us={restart_start_latency_us}, chord_frames={chord_frames}, release_frames={release_frames}, idle_frames={idle_frames}, restart_frames={restart_frames}, frames_submitted={frames}, peak={peak:.4}, errors={}",
            engine.diagnostics.buffer_frames.load(Ordering::Relaxed),
            engine.diagnostics.period_frames.load(Ordering::Relaxed),
            engine.diagnostics.errors.load(Ordering::Relaxed),
        );
        assert!(peak > 0.0);
        assert!(
            first_buffer_latency_us > 0,
            "no first-buffer timing was recorded"
        );
        assert!(
            first_start_latency_us > 0,
            "no first Start timing was recorded"
        );
        assert!(
            first_start_latency_us < 50_000,
            "first note Start took {first_start_latency_us} µs after its event"
        );
        assert!(
            restart_start_latency_us > 0 && restart_start_latency_us < 50_000,
            "restarted note Start took {restart_start_latency_us} µs after its event"
        );
        assert_eq!(
            engine.diagnostics.errors.load(Ordering::Relaxed),
            0,
            "WASAPI reported errors during the test"
        );
        drop(engine);
        assert_eq!(shared.wake_handle.load(Ordering::Acquire), 0);
    }
}
