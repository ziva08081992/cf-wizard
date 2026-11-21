//! Cooperative guard for memory-intensive operations in the Windows desktop app.
use cf_application::Cancellation;
use cf_domain::Result;

const MIB: u64 = 1024 * 1024;

pub(crate) fn budget_advice(manual_mib: Option<u32>) -> Option<(u64, bool)> {
    #[cfg(windows)]
    {
        let available = windows::available_physical()?;
        let warn = manual_mib.is_some_and(|mib| warn_at_budget(mib, available));
        Some((available / MIB, warn))
    }
    #[cfg(not(windows))]
    {
        let _ = manual_mib;
        None
    }
}

#[cfg(any(windows, test))]
fn warn_at_budget(manual_mib: u32, available_bytes: u64) -> bool {
    u128::from(manual_mib) * u128::from(MIB) * 5 >= u128::from(available_bytes) * 2
}

#[cfg(test)]
#[test]
fn budget_warning_includes_exactly_forty_percent_of_available_ram() {
    let available = 10_240 * MIB;
    assert!(!warn_at_budget(4095, available));
    assert!(warn_at_budget(4096, available));
    assert!(!warn_at_budget(4096, available + 1));
    assert!(warn_at_budget(4097, available));
}

pub(crate) struct PreviewBudget {
    pub cache_bytes: u64,
    process_target: u64,
}

impl PreviewBudget {
    pub(crate) fn additional_bytes(&self) -> Option<u64> {
        #[cfg(windows)]
        return windows::preview_headroom(self.process_target);
        #[cfg(not(windows))]
        Some(self.process_target)
    }
}

pub(crate) fn preview_budget(manual_mib: Option<u32>) -> Result<Option<PreviewBudget>> {
    #[cfg(windows)]
    return windows::preview_budget(manual_mib);
    #[cfg(not(windows))]
    Ok(manual_mib.map(|mib| {
        let process_target = u64::from(mib) * MIB;
        PreviewBudget {
            cache_bytes: process_target / 2,
            process_target,
        }
    }))
}

#[derive(Clone, Copy)]
pub(crate) enum Completion {
    UnpublishedPreview,
    PreserveResult,
}

#[cfg(windows)]
mod windows {
    use super::{Completion, MIB, PreviewBudget};
    use cf_application::Cancellation;
    use cf_domain::{Error, Result};
    use std::{mem::size_of, sync::mpsc, thread, time::Duration};

    const GIB: u64 = 1024 * MIB;
    const INTERVAL: Duration = Duration::from_millis(125);
    const COMMIT_RESERVE: u64 = 512 * MIB;
    const CRITICAL: u64 = 192 * MIB;

    #[derive(Clone, Copy)]
    struct Memory {
        total_physical: u64,
        available_physical: u64,
        available_commit: u64,
        process_private: Option<u64>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Pressure {
        Physical,
        Commit,
        OperationBudget,
    }

    #[derive(Clone, Copy)]
    struct Budget {
        physical_reserve: u64,
        private_limit: Option<u64>,
    }

    impl Budget {
        fn new(start: Memory) -> Self {
            let physical_reserve = physical_reserve(start.total_physical);
            let private_limit = if start.total_physical <= 12 * GIB {
                start.process_private.map(|initial| {
                    initial
                        .saturating_add(start.available_physical.saturating_sub(physical_reserve))
                })
            } else {
                None
            };
            Self {
                physical_reserve,
                private_limit,
            }
        }

        fn pressure(self, memory: Memory) -> Option<Pressure> {
            if memory.available_physical < self.physical_reserve {
                return Some(Pressure::Physical);
            }
            if memory.available_commit < COMMIT_RESERVE {
                return Some(Pressure::Commit);
            }
            if self
                .private_limit
                .zip(memory.process_private)
                .is_some_and(|(limit, used)| used > limit)
            {
                return Some(Pressure::OperationBudget);
            }
            None
        }

        fn error(self, memory: Memory, reason: Pressure) -> Error {
            let detail = match reason {
                Pressure::Physical => format!(
                    "Windows доступно {} МиБ ОЗУ, нужно оставить не менее {} МиБ",
                    memory.available_physical / MIB,
                    self.physical_reserve / MIB
                ),
                Pressure::Commit => format!(
                    "доступный резерв выделения памяти Windows — {} МиБ",
                    memory.available_commit / MIB
                ),
                Pressure::OperationBudget => format!(
                    "мастер использовал {} МиБ частной памяти при доступном бюджете {} МиБ",
                    memory.process_private.unwrap_or_default() / MIB,
                    self.private_limit.unwrap_or_default() / MIB
                ),
            };
            Error::Invalid(format!(
                "Операция остановлена из-за нехватки памяти. {detail}. Закройте лишние программы или сократите число выбранных объектов и повторите."
            ))
        }
    }

    #[repr(C)]
    #[derive(Default)]
    struct MemoryStatus {
        length: u32,
        load: u32,
        total_physical: u64,
        available_physical: u64,
        total_page_file: u64,
        available_page_file: u64,
        total_virtual: u64,
        available_virtual: u64,
        available_extended_virtual: u64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ProcessCounters {
        cb: u32,
        page_fault_count: u32,
        peak_working_set: usize,
        working_set: usize,
        peak_paged_pool: usize,
        paged_pool: usize,
        peak_nonpaged_pool: usize,
        nonpaged_pool: usize,
        pagefile_usage: usize,
        peak_pagefile_usage: usize,
        private_usage: usize,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalMemoryStatusEx(status: *mut MemoryStatus) -> i32;
        fn GetCurrentProcess() -> *mut std::ffi::c_void;
        fn K32GetProcessMemoryInfo(
            process: *mut std::ffi::c_void,
            counters: *mut ProcessCounters,
            size: u32,
        ) -> i32;
    }

    fn system_memory() -> Option<MemoryStatus> {
        let mut status = MemoryStatus {
            length: size_of::<MemoryStatus>() as u32,
            ..Default::default()
        };
        // Windows writes into an initialized structure of the documented size.
        if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 || status.total_physical == 0 {
            return None;
        }
        Some(status)
    }

    pub(super) fn available_physical() -> Option<u64> {
        system_memory().map(|status| status.available_physical)
    }

    fn sample() -> Option<Memory> {
        let status = system_memory()?;
        let size = size_of::<ProcessCounters>() as u32;
        let mut counters = ProcessCounters {
            cb: size,
            ..Default::default()
        };
        let available =
            unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) } != 0;
        Some(Memory {
            total_physical: status.total_physical,
            available_physical: status.available_physical,
            available_commit: status.available_page_file,
            process_private: available.then_some(counters.private_usage as u64),
        })
    }

    fn physical_reserve(total_physical: u64) -> u64 {
        (total_physical / 8).clamp(384 * MIB, GIB)
    }

    fn headroom(memory: Memory, process_target: u64) -> Option<u64> {
        Some(
            process_target
                .saturating_sub(memory.process_private?)
                .min(
                    memory
                        .available_physical
                        .saturating_sub(physical_reserve(memory.total_physical)),
                )
                .min(memory.available_commit.saturating_sub(COMMIT_RESERVE)),
        )
    }

    pub(super) fn preview_headroom(process_target: u64) -> Option<u64> {
        headroom(sample()?, process_target)
    }

    fn preview_budget_for(
        initial: Option<Memory>,
        manual_mib: Option<u32>,
    ) -> Result<Option<PreviewBudget>> {
        let manual = manual_mib.map(|mib| u64::from(mib) * MIB);
        let adaptive = initial.and_then(|memory| Budget::new(memory).private_limit);
        let target = match (manual, adaptive) {
            (Some(manual), Some(adaptive)) => manual.min(adaptive),
            (Some(manual), None) => manual,
            (None, Some(adaptive)) => adaptive,
            (None, None) => return Ok(None),
        };
        if let Some((mib, used)) = manual_mib.zip(initial.and_then(|memory| memory.process_private))
            && used >= u64::from(mib) * MIB
        {
            return Err(Error::Invalid(format!(
                "Для запуска предпросмотра не хватает рабочего бюджета. Мастер уже занимает {} МиБ, задано {mib} МиБ. Увеличьте бюджет или включите автоматический режим.",
                used.div_ceil(MIB)
            )));
        }
        let available = initial
            .and_then(|memory| headroom(memory, target))
            .unwrap_or(target);
        // Other allocations, including the dependency graph and the plan, share the same process.
        Ok(Some(PreviewBudget {
            cache_bytes: available / 2,
            process_target: target,
        }))
    }

    pub(super) fn preview_budget(manual_mib: Option<u32>) -> Result<Option<PreviewBudget>> {
        preview_budget_for(sample(), manual_mib)
    }

    pub(super) fn run<T>(
        cancel: &Cancellation,
        completion: Completion,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let Some(initial) = sample() else {
            return operation();
        };
        run_guarded(cancel, completion, initial, sample, operation)
    }

    fn run_guarded<T>(
        cancel: &Cancellation,
        completion: Completion,
        initial: Memory,
        mut read: impl FnMut() -> Option<Memory> + Send,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        cancel.check()?;
        let budget = Budget::new(initial);
        if let Some(reason) = budget.pressure(initial) {
            return Err(budget.error(initial, reason));
        }
        thread::scope(|scope| {
            let (stop, finished) = mpsc::channel::<()>();
            let watch = thread::Builder::new()
                .name("cf-memory".into())
                .spawn_scoped(scope, move || {
                    let mut previous_high = false;
                    while let Err(mpsc::RecvTimeoutError::Timeout) = finished.recv_timeout(INTERVAL)
                    {
                        if cancel.check().is_err() {
                            return None;
                        }
                        let Some(memory) = read() else {
                            previous_high = false;
                            continue;
                        };
                        let pressure = budget.pressure(memory);
                        if let Some(reason) = pressure
                            && (previous_high
                                || memory.available_physical < CRITICAL
                                || memory.available_commit < CRITICAL)
                        {
                            cancel.cancel();
                            return Some((memory, reason));
                        }
                        previous_high = pressure.is_some();
                    }
                    None
                })
                .map_err(|error| {
                    Error::External(format!("Не удалось запустить контроль памяти: {error}"))
                })?;
            let result = operation();
            let _ = stop.send(());
            let pressure = watch.join().ok().flatten();
            match (result, pressure) {
                (Err(Error::Cancelled), Some((memory, reason))) => {
                    Err(budget.error(memory, reason))
                }
                (Ok(_), Some((memory, reason)))
                    if matches!(completion, Completion::UnpublishedPreview) =>
                {
                    Err(budget.error(memory, reason))
                }
                (result, _) => result,
            }
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn memory(total: u64, free: u64, private: u64) -> Memory {
            Memory {
                total_physical: total,
                available_physical: free,
                available_commit: 8 * GIB,
                process_private: Some(private),
            }
        }

        #[test]
        fn small_system_uses_initial_physical_headroom_for_operation() {
            let budget = Budget::new(memory(8 * GIB, 4 * GIB, GIB));
            assert_eq!(budget.private_limit, Some(4 * GIB));
            assert_eq!(budget.pressure(memory(8 * GIB, 3 * GIB, 4 * GIB)), None);
            assert_eq!(
                budget.pressure(memory(8 * GIB, 3 * GIB, 4 * GIB + 1)),
                Some(Pressure::OperationBudget)
            );
        }

        #[test]
        fn working_budget_respects_process_target_and_system_reserve() {
            let initial = memory(16 * GIB, 3 * GIB, GIB);
            let budget = preview_budget_for(Some(initial), Some(4096))
                .unwrap()
                .unwrap();
            assert_eq!(budget.cache_bytes, GIB);
            assert_eq!(
                headroom(memory(16 * GIB, 2 * GIB, 3500 * MIB), budget.process_target),
                Some(596 * MIB)
            );
            assert!(preview_budget_for(Some(initial), Some(1024)).is_err());

            let small = memory(8 * GIB, 4 * GIB, GIB);
            assert_eq!(
                preview_budget_for(Some(small), None)
                    .unwrap()
                    .unwrap()
                    .cache_bytes,
                1536 * MIB
            );
        }

        #[test]
        fn ordinary_system_keeps_existing_mode_until_system_pressure() {
            let budget = Budget::new(memory(16 * GIB, 10 * GIB, GIB));
            assert_eq!(budget.private_limit, None);
            assert_eq!(budget.pressure(memory(16 * GIB, 3 * GIB, 8 * GIB)), None);
            assert_eq!(
                budget.pressure(memory(16 * GIB, GIB - 1, 3 * GIB)),
                Some(Pressure::Physical)
            );
            let mut current = memory(32 * GIB, 8 * GIB, 3 * GIB);
            current.available_commit = 300 * MIB;
            assert_eq!(budget.pressure(current), Some(Pressure::Commit));
        }

        #[test]
        fn reports_guard_cancellation_and_rejects_operation_when_already_low() {
            let cancel = Cancellation::default();
            let starting = memory(8 * GIB, 4 * GIB, GIB);
            let pressure = memory(8 * GIB, 128 * MIB, GIB);
            let result = run_guarded::<()>(
                &cancel,
                Completion::PreserveResult,
                starting,
                || Some(pressure),
                || {
                    let deadline = std::time::Instant::now() + Duration::from_secs(3);
                    while std::time::Instant::now() < deadline {
                        cancel.check()?;
                        thread::sleep(Duration::from_millis(5));
                    }
                    panic!("memory watcher did not request cancellation")
                },
            );
            assert!(matches!(result, Err(Error::Invalid(message)) if message.contains("128 МиБ")));

            let cancel = Cancellation::default();
            let result = run_guarded::<()>(
                &cancel,
                Completion::PreserveResult,
                pressure,
                || None,
                || panic!("operation must not begin without a reserve"),
            );
            assert!(matches!(result, Err(Error::Invalid(message)) if message.contains("128 МиБ")));
        }

        #[test]
        fn changing_pressure_reason_still_cancels_an_unpublished_preview_only() {
            for completion in [Completion::UnpublishedPreview, Completion::PreserveResult] {
                let cancel = Cancellation::default();
                let starting = memory(8 * GIB, 4 * GIB, GIB);
                let mut samples = [
                    memory(8 * GIB, 3 * GIB, 5 * GIB),
                    memory(8 * GIB, 512 * MIB, 5 * GIB),
                ]
                .into_iter();
                let result = run_guarded(
                    &cancel,
                    completion,
                    starting,
                    move || samples.next(),
                    || {
                        let deadline = std::time::Instant::now() + Duration::from_secs(3);
                        while std::time::Instant::now() < deadline {
                            if cancel.check().is_err() {
                                return Ok(());
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                        panic!("consecutive over-budget samples did not request cancellation")
                    },
                );
                match completion {
                    Completion::UnpublishedPreview => {
                        assert!(
                            matches!(result, Err(Error::Invalid(message)) if message.contains("512 МиБ"))
                        );
                    }
                    Completion::PreserveResult => assert!(result.is_ok()),
                }
            }
        }

        #[test]
        fn manual_cancellation_keeps_its_reason() {
            let cancel = Cancellation::default();
            let starting = memory(16 * GIB, 8 * GIB, GIB);
            let result = run_guarded::<()>(
                &cancel,
                Completion::PreserveResult,
                starting,
                || Some(starting),
                || {
                    cancel.cancel();
                    cancel.check()
                },
            );
            assert!(matches!(result, Err(Error::Cancelled)));
        }
    }
}

pub(crate) fn run<T>(
    cancel: &Cancellation,
    completion: Completion,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    #[cfg(windows)]
    return windows::run(cancel, completion, operation);
    #[cfg(not(windows))]
    {
        let _ = (cancel, completion);
        operation()
    }
}
