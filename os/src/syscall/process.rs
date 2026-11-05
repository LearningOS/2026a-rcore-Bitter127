//! Process management syscalls
use crate::config::PAGE_SIZE;
use crate::mm::{MapPermission, PageTable, PhysAddr, VirtAddr};
use crate::task::{
    change_program_brk, current_user_token, exit_current_and_run_next, map_current_range,
    suspend_current_and_run_next, syscall_count, unmap_current_range,
};
use crate::timer::get_time_us;

const USER_VA_LIMIT: usize = 1 << 38;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// Get time with second and microsecond, writing through the user page table.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel: sys_get_time");
    let us = get_time_us();
    let value = TimeVal {
        sec: us / 1_000_000,
        usec: us % 1_000_000,
    };
    let bytes = unsafe {
        core::slice::from_raw_parts(
            &value as *const TimeVal as *const u8,
            core::mem::size_of::<TimeVal>(),
        )
    };
    if copy_to_user(ts as usize, bytes) {
        0
    } else {
        -1
    }
}

/// Read or write one user byte, or query a syscall counter.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    trace!("kernel: sys_trace");
    match trace_request {
        0 => translate_user_address(id, false)
            .map(|pa| *pa.get_mut::<u8>() as isize)
            .unwrap_or(-1),
        1 => {
            if let Some(pa) = translate_user_address(id, true) {
                *pa.get_mut::<u8>() = data as u8;
                0
            } else {
                -1
            }
        }
        2 => syscall_count(id).map_or(-1, |count| count as isize),
        _ => -1,
    }
}

/// Map anonymous pages into the current user address space.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!("kernel: sys_mmap");
    let Some(end) = valid_user_range(start, len) else {
        return -1;
    };
    if start % PAGE_SIZE != 0 || prot == 0 || prot & !0x7 != 0 {
        return -1;
    }
    let mut permission = MapPermission::U;
    if prot & 1 != 0 {
        permission |= MapPermission::R;
    }
    if prot & 2 != 0 {
        permission |= MapPermission::W;
    }
    if prot & 4 != 0 {
        permission |= MapPermission::X;
    }
    if map_current_range(start.into(), end.into(), permission) {
        0
    } else {
        -1
    }
}

/// Remove pages previously created by `sys_mmap`.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel: sys_munmap");
    let Some(end) = valid_user_range(start, len) else {
        return -1;
    };
    if start % PAGE_SIZE != 0 {
        return -1;
    }
    if unmap_current_range(start.into(), end.into()) {
        0
    } else {
        -1
    }
}
/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

fn valid_user_range(start: usize, len: usize) -> Option<usize> {
    if len == 0 || start >= USER_VA_LIMIT {
        return None;
    }
    start.checked_add(len).filter(|end| *end <= USER_VA_LIMIT)
}

fn translate_user_address(address: usize, write: bool) -> Option<PhysAddr> {
    if address >= USER_VA_LIMIT {
        return None;
    }
    let va = VirtAddr::from(address);
    let pte = PageTable::from_token(current_user_token()).translate(va.floor())?;
    if !pte.is_valid()
        || !pte.user_accessible()
        || if write {
            !pte.writable()
        } else {
            !pte.readable()
        }
    {
        return None;
    }
    let page_base: PhysAddr = pte.ppn().into();
    Some((usize::from(page_base) + va.page_offset()).into())
}

fn copy_to_user(destination: usize, bytes: &[u8]) -> bool {
    let Some(_) = valid_user_range(destination, bytes.len()) else {
        return false;
    };
    if !(0..bytes.len()).all(|offset| translate_user_address(destination + offset, true).is_some())
    {
        return false;
    }
    for (offset, byte) in bytes.iter().enumerate() {
        let pa = translate_user_address(destination + offset, true).unwrap();
        *pa.get_mut::<u8>() = *byte;
    }
    true
}
