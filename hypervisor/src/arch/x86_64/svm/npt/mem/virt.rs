use crate::arch::x86_64::{guest_memory, memory::PAGE_SIZE};

use super::error::MemError;
use super::phys::{read_gpa, write_gpa};

const USER: bool = false;

pub(super) fn translate(cr3: u64, va: u64) -> Result<u64, MemError> {
    Ok(guest_memory::translate_guest(cr3, va, USER, false, false)?)
}

pub(crate) fn read_struct<T: Copy>(cr3: u64, address: u64) -> Result<T, MemError> {
    let size = core::mem::size_of::<T>();
    let mut value = core::mem::MaybeUninit::<T>::uninit();

    // SAFETY: `T` is Copy and the destination is exactly `size` bytes.
    let bytes = unsafe { core::slice::from_raw_parts_mut(value.as_mut_ptr() as *mut u8, size) };
    read_bytes(cr3, address, bytes)?;

    Ok(unsafe { value.assume_init() })
}

pub(crate) fn write_struct<T: Copy>(cr3: u64, address: u64, value: &T) -> Result<(), MemError> {
    let size = core::mem::size_of::<T>();
    let bytes =
        unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size) };
    write_bytes(cr3, address, bytes)
}

pub(crate) fn read_bytes(cr3: u64, address: u64, destination: &mut [u8]) -> Result<(), MemError> {
    let mut done = 0usize;

    while done < destination.len() {
        let gpa = translate(cr3, address.wrapping_add(done as u64))?;
        let offset = (gpa & 0xfff) as usize;
        let chunk = (destination.len() - done).min(PAGE_SIZE - offset);

        read_gpa(gpa, &mut destination[done..done + chunk])?;
        done += chunk;
    }

    Ok(())
}

pub(crate) fn write_bytes(cr3: u64, address: u64, source: &[u8]) -> Result<(), MemError> {
    let mut done = 0usize;

    while done < source.len() {
        let gpa = translate(cr3, address.wrapping_add(done as u64))?;
        let offset = (gpa & 0xfff) as usize;
        let chunk = (source.len() - done).min(PAGE_SIZE - offset);

        write_gpa(gpa, &source[done..done + chunk])?;
        done += chunk;
    }

    Ok(())
}

pub(super) fn read_virt(cr3: u64, address: u64, destination: &mut [u8]) -> Result<(), MemError> {
    read_bytes(cr3, address, destination)
}

pub(super) fn write_virt(cr3: u64, address: u64, source: &[u8]) -> Result<(), MemError> {
    write_bytes(cr3, address, source)
}
