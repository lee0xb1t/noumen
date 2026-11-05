#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtAddr {
    addr: u64,
}

const _: () = assert!(size_of::<VirtAddr>() == 8);

impl VirtAddr {
    pub const fn new(addr: u64) -> Self {
        Self { addr }
    }

    pub const fn null() -> Self {
        Self { addr: 0 }
    }

    pub fn from_ptr<T>(ptr: *const T) -> Self {
        Self::new(ptr as u64)
    }

    pub const fn as_ptr<T>(&self) -> *const T {
        self.addr as *const T
    }

    pub const fn as_mut_ptr<T>(&self) -> *mut T {
        self.addr as *mut T
    }

    pub const fn as_u64(&self) -> u64 {
        self.addr
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct PhysAddr(u64);

impl PhysAddr {
    pub fn new(addr: u64) -> Self {
        Self(addr)
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }
}
