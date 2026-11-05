use crate::arch::x86_64::{
    addr::PhysAddr,
    paging::{self, DIRECT_MAP_SIZE},
    safe_routine::{self, HostExceptionInfo},
};

use super::error::MemError;

pub(crate) fn read_gpa(gpa: u64, destination: &mut [u8]) -> Result<(), MemError> {
    let Some(end) = gpa.checked_add(destination.len() as u64) else {
        return Err(MemError::BadEntry);
    };

    if end > DIRECT_MAP_SIZE {
        return Err(MemError::OutOfMap);
    }

    let source = paging::phys_to_virt(PhysAddr::new(gpa)).as_ptr::<u8>();
    let mut info = HostExceptionInfo::new();

    if safe_routine::memcpy_safe(&mut info, destination.as_mut_ptr(), source, destination.len()) {
        Ok(())
    } else {
        Err(MemError::HostFault)
    }
}

pub(super) fn write_gpa(gpa: u64, source: &[u8]) -> Result<(), MemError> {
    let Some(end) = gpa.checked_add(source.len() as u64) else {
        return Err(MemError::BadEntry);
    };

    if end > DIRECT_MAP_SIZE {
        return Err(MemError::OutOfMap);
    }

    let destination = paging::phys_to_virt(PhysAddr::new(gpa)).as_mut_ptr::<u8>();
    let mut info = HostExceptionInfo::new();

    if safe_routine::memcpy_safe(&mut info, destination, source.as_ptr(), source.len()) {
        Ok(())
    } else {
        Err(MemError::HostFault)
    }
}
