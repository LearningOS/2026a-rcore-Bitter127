use crate::{
    config::PAGE_SIZE,
    fs::{open_file, OpenFlags},
    mm::{
        translated_ref, translated_refmut, translated_str, MapPermission, PageTable, PhysAddr,
        VirtAddr,
    },
    task::{
        current_process, current_task, current_user_token, exit_current_and_run_next, pid2process,
        suspend_current_and_run_next, SignalFlags,
    },
    timer::get_time_us,
};
use alloc::{string::String, sync::Arc, vec::Vec};

const USER_VA_LIMIT: usize = 1 << 38;

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// exit syscall
///
/// exit the current task and run the next task in task list
pub fn sys_exit(exit_code: i32) -> ! {
    trace!(
        "kernel:pid[{}] sys_exit",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}
/// yield syscall
pub fn sys_yield() -> isize {
    //trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}
/// getpid syscall
pub fn sys_getpid() -> isize {
    trace!(
        "kernel: sys_getpid pid:{}",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    current_task().unwrap().process.upgrade().unwrap().getpid() as isize
}
/// fork child process syscall
pub fn sys_fork() -> isize {
    trace!(
        "kernel:pid[{}] sys_fork",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let current_process = current_process();
    let new_process = current_process.fork();
    let new_pid = new_process.getpid();
    // modify trap context of new_task, because it returns immediately after switching
    let new_process_inner = new_process.inner_exclusive_access();
    let task = new_process_inner.tasks[0].as_ref().unwrap();
    let trap_cx = task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    new_pid as isize
}
/// exec syscall
pub fn sys_exec(path: *const u8, mut args: *const usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_exec",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let token = current_user_token();
    let path = translated_str(token, path);
    let mut args_vec: Vec<String> = Vec::new();
    loop {
        let arg_str_ptr = *translated_ref(token, args);
        if arg_str_ptr == 0 {
            break;
        }
        args_vec.push(translated_str(token, arg_str_ptr as *const u8));
        unsafe {
            args = args.add(1);
        }
    }
    if let Some(app_inode) = open_file(path.as_str(), OpenFlags::RDONLY) {
        let all_data = app_inode.read_all();
        let process = current_process();
        let argc = args_vec.len();
        process.exec(all_data.as_slice(), args_vec);
        // return argc because cx.x[10] will be covered with it later
        argc as isize
    } else {
        -1
    }
}

/// waitpid syscall
///
/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    //trace!("kernel: sys_waitpid");
    let process = current_process();
    // find a child process

    let mut inner = process.inner_exclusive_access();
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
        p.inner_exclusive_access().is_zombie && (pid == -1 || pid as usize == p.getpid())
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

/// kill syscall
pub fn sys_kill(pid: usize, signal: u32) -> isize {
    trace!(
        "kernel:pid[{}] sys_kill",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    if let Some(process) = pid2process(pid) {
        if let Some(flag) = SignalFlags::from_bits(signal) {
            process.inner_exclusive_access().signals |= flag;
            0
        } else {
            -1
        }
    } else {
        -1
    }
}

/// get_time syscall
///
/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
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

/// mmap syscall
///
/// YOUR JOB: Implement mmap.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
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
    let process = current_process();
    let ok = process.inner_exclusive_access().memory_set.map_user_range(
        start.into(),
        end.into(),
        permission,
    );
    if ok {
        0
    } else {
        -1
    }
}

/// munmap syscall
///
/// YOUR JOB: Implement munmap.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    let Some(end) = valid_user_range(start, len) else {
        return -1;
    };
    if start % PAGE_SIZE != 0 {
        return -1;
    }
    let process = current_process();
    let ok = process
        .inner_exclusive_access()
        .memory_set
        .unmap_user_range(start.into(), end.into());
    if ok {
        0
    } else {
        -1
    }
}

/// change data segment size
// pub fn sys_sbrk(size: i32) -> isize {
//     trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().process.upgrade().unwrap().getpid());
//     if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
//         old_brk as isize
//     } else {
//     -1
// }

/// spawn syscall
/// YOUR JOB: Implement spawn.
/// HINT: fork + exec =/= spawn
pub fn sys_spawn(path: *const u8) -> isize {
    let parent = current_process();
    let path = translated_str(current_user_token(), path);
    let Some(inode) = open_file(path.as_str(), OpenFlags::RDONLY) else {
        return -1;
    };
    let child = crate::task::ProcessControlBlock::new(inode.read_all().as_slice());
    let pid = child.getpid();
    child.inner_exclusive_access().parent = Some(Arc::downgrade(&parent));
    parent.inner_exclusive_access().children.push(child);
    pid as isize
}

/// set priority syscall
///
/// YOUR JOB: Set task priority
pub fn sys_set_priority(priority: isize) -> isize {
    if priority < 2 {
        return -1;
    }
    current_task().unwrap().inner_exclusive_access().priority = priority as usize;
    priority
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
    let entry = PageTable::from_token(current_user_token()).translate(va.floor())?;
    if !entry.is_valid()
        || !entry.user_accessible()
        || if write {
            !entry.writable()
        } else {
            !entry.readable()
        }
    {
        return None;
    }
    let page_base: PhysAddr = entry.ppn().into();
    Some((usize::from(page_base) + va.page_offset()).into())
}

fn copy_to_user(destination: usize, bytes: &[u8]) -> bool {
    if valid_user_range(destination, bytes.len()).is_none() {
        return false;
    }
    if !(0..bytes.len()).all(|offset| translate_user_address(destination + offset, true).is_some())
    {
        return false;
    }
    for (offset, byte) in bytes.iter().enumerate() {
        *translate_user_address(destination + offset, true)
            .unwrap()
            .get_mut::<u8>() = *byte;
    }
    true
}
