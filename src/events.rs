use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, AtomicU32, AtomicUsize, Ordering};

use crate::settings::Settings;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyEvent {
    pub key_id: u16,
    pub down: bool,
}

const EVENT_CAPACITY: usize = 1024;

/// A bounded single-producer/single-consumer ring. One producer and one consumer
/// may access it concurrently; producer and consumer indices are monotonic.
pub struct EventQueue {
    slots: [UnsafeCell<MaybeUninit<KeyEvent>>; EVENT_CAPACITY],
    head: AtomicUsize,
    tail: AtomicUsize,
    pub overflow: AtomicBool,
}

unsafe impl Sync for EventQueue {}
unsafe impl Send for EventQueue {}

impl EventQueue {
    pub fn new() -> Self {
        Self {
            slots: [const { UnsafeCell::new(MaybeUninit::uninit()) }; EVENT_CAPACITY],
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            overflow: AtomicBool::new(false),
        }
    }

    pub fn push(&self, event: KeyEvent) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) >= EVENT_CAPACITY {
            self.overflow.store(true, Ordering::Release);
            return false;
        }
        // SAFETY: SPSC ownership gives the producer exclusive access to this slot
        // until it publishes the new head with Release ordering.
        unsafe {
            (*self.slots[head % EVENT_CAPACITY].get()).write(event);
        }
        self.head.store(head.wrapping_add(1), Ordering::Release);
        true
    }

    pub fn pop(&self) -> Option<KeyEvent> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        // SAFETY: the Acquire load observes the producer's completed write; the
        // single consumer exclusively owns this slot until publishing tail.
        let event = unsafe { (*self.slots[tail % EVENT_CAPACITY].get()).assume_init_read() };
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(event)
    }

    pub fn take_overflow(&self) -> bool {
        self.overflow.swap(false, Ordering::AcqRel)
    }
}

impl Default for EventQueue {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Shared {
    pub queue: EventQueue,
    pub overflow: AtomicBool,
    pub enabled: AtomicBool,
    pub volume: AtomicU32,
    pub transpose: AtomicI32,
    pub waveform: AtomicU32,
    pub mode: AtomicU32,
    pub attack_ms: AtomicU32,
    pub release_ms: AtomicU32,
    pub shutdown: AtomicBool,
    pub wake_handle: AtomicIsize,
    pub window_handle: AtomicIsize,
}

impl Shared {
    pub fn new(settings: &Settings) -> Self {
        Self {
            queue: EventQueue::new(),
            overflow: AtomicBool::new(false),
            enabled: AtomicBool::new(settings.enabled),
            volume: AtomicU32::new(settings.volume.min(100)),
            transpose: AtomicI32::new(settings.transpose.clamp(-24, 24)),
            waveform: AtomicU32::new(settings.waveform as u32),
            mode: AtomicU32::new(settings.mode as u32),
            attack_ms: AtomicU32::new(settings.attack_ms.min(200)),
            release_ms: AtomicU32::new(settings.release_ms.min(500)),
            shutdown: AtomicBool::new(false),
            wake_handle: AtomicIsize::new(0),
            window_handle: AtomicIsize::new(0),
        }
    }

    pub fn apply(&self, settings: &Settings) {
        self.enabled.store(settings.enabled, Ordering::Release);
        self.volume
            .store(settings.volume.min(100), Ordering::Release);
        self.transpose
            .store(settings.transpose.clamp(-24, 24), Ordering::Release);
        self.waveform
            .store(settings.waveform as u32, Ordering::Release);
        self.mode.store(settings.mode as u32, Ordering::Release);
        self.attack_ms
            .store(settings.attack_ms.min(200), Ordering::Release);
        self.release_ms
            .store(settings.release_ms.min(500), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_preserves_order_and_reports_full() {
        let queue = EventQueue::new();
        for i in 0..EVENT_CAPACITY {
            assert!(queue.push(KeyEvent {
                key_id: i as u16,
                down: i % 2 == 0
            }));
        }
        assert!(!queue.push(KeyEvent {
            key_id: 999,
            down: true
        }));
        assert!(queue.take_overflow());
        assert!(!queue.take_overflow());
        for i in 0..EVENT_CAPACITY {
            assert_eq!(
                queue.pop(),
                Some(KeyEvent {
                    key_id: i as u16,
                    down: i % 2 == 0
                })
            );
        }
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn shared_applies_bounded_parameters() {
        let shared = Shared::new(&Settings::default());
        let settings = Settings {
            volume: 130,
            transpose: 80,
            attack_ms: 20_000,
            ..Settings::default()
        };
        shared.apply(&settings);
        assert_eq!(shared.volume.load(Ordering::Relaxed), 100);
        assert_eq!(shared.transpose.load(Ordering::Relaxed), 24);
        assert_eq!(shared.attack_ms.load(Ordering::Relaxed), 200);
    }

    #[test]
    fn queue_preserves_order_across_concurrent_producer_and_consumer() {
        use std::sync::Arc;
        use std::thread;

        const EVENTS: usize = 20_000;
        let queue = Arc::new(EventQueue::new());
        let producer_queue = Arc::clone(&queue);
        let producer = thread::spawn(move || {
            for i in 0..EVENTS {
                let event = KeyEvent {
                    key_id: (i % 512) as u16,
                    down: i % 2 == 0,
                };
                while !producer_queue.push(event) {
                    thread::yield_now();
                }
            }
        });
        for i in 0..EVENTS {
            let expected = KeyEvent {
                key_id: (i % 512) as u16,
                down: i % 2 == 0,
            };
            loop {
                if let Some(actual) = queue.pop() {
                    assert_eq!(actual, expected);
                    break;
                }
                thread::yield_now();
            }
        }
        producer.join().unwrap();
        assert_eq!(queue.pop(), None);
    }
}
