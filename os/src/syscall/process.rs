//! Process management syscalls
//!
use alloc::sync::Arc;

use crate::{
    config::PAGE_SIZE,
    fs::{open_file, OpenFlags},
    mm::{translated_refmut, translated_str, MapPermission, PageTable, PhysAddr, VirtAddr},
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next,
    },
    timer::get_time_us,
};

const USER_VA_LIMIT: usize = 1 << 38;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

pub fn sys_fork() -> isize {
    trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let task = current_task().unwrap();
        task.exec(all_data.as_slice());
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}

/// Get time with second and microsecond, writing through the user page table.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    trace!("kernel:pid[{}] sys_get_time", current_task().unwrap().pid.0);
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

/// Map anonymous pages into the current process.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    trace!("kernel:pid[{}] sys_mmap", current_task().unwrap().pid.0);
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
    if current_task()
        .unwrap()
        .map_user_range(start.into(), end.into(), permission)
    {
        0
    } else {
        -1
    }
}

/// Remove pages previously created by `sys_mmap`.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    trace!("kernel:pid[{}] sys_munmap", current_task().unwrap().pid.0);
    let Some(end) = valid_user_range(start, len) else {
        return -1;
    };
    if start % PAGE_SIZE != 0 {
        return -1;
    }
    if current_task()
        .unwrap()
        .unmap_user_range(start.into(), end.into())
    {
        0
    } else {
        -1
    }
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// Create a child process directly from an executable stored in easy-fs.
pub fn sys_spawn(path: *const u8) -> isize {
    let parent = current_task().unwrap();
    trace!("kernel:pid[{}] sys_spawn", parent.pid.0);
    let path = translated_str(parent.get_user_token(), path);
    let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) else {
        return -1;
    };
    let data = app_inode.read_all();
    let child = Arc::new(crate::task::TaskControlBlock::new(data.as_slice()));
    let child_pid = child.pid.0;
    child.inner_exclusive_access().parent = Some(Arc::downgrade(&parent));
    parent.inner_exclusive_access().children.push(child.clone());
    add_task(child);
    child_pid as isize
}

/// Set the current process's stride scheduling priority.
pub fn sys_set_priority(priority: isize) -> isize {
    let task = current_task().unwrap();
    trace!("kernel:pid[{}] sys_set_priority", task.pid.0);
    task.set_priority(priority)
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
