//! Process management syscalls
use crate::{
    task::{exit_current_and_run_next, suspend_current_and_run_next, syscall_count},
    timer::get_time_us,
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("[kernel] Application exited with code {}", exit_code);
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// get time with second and microsecond
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    unsafe {
        *ts = TimeVal {
            sec: us / 1_000_000,
            usec: us % 1_000_000,
        };
    }
    0
}

/// Inspect or update one byte in the flat user address space, or query a
/// syscall counter maintained by the task manager.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    match trace_request {
        0 => unsafe { (id as *const u8).read_volatile() as isize },
        1 => {
            unsafe { (id as *mut u8).write_volatile(data as u8) };
            0
        }
        2 => syscall_count(id).map_or(-1, |count| count as isize),
        _ => -1,
    }
}
