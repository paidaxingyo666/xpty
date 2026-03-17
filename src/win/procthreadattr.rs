use crate::error::{Error, Result};
use std::io::Error as IoError;
use std::{mem, ptr};
use windows_sys::Win32::System::Console::HPCON;
use windows_sys::Win32::System::Threading::{
    DeleteProcThreadAttributeList, InitializeProcThreadAttributeList, UpdateProcThreadAttribute,
    LPPROC_THREAD_ATTRIBUTE_LIST,
};

const PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE: usize = 0x00020016;

pub struct ProcThreadAttributeList {
    data: Vec<u8>,
}

impl ProcThreadAttributeList {
    pub fn with_capacity(num_attributes: u32) -> Result<Self> {
        let mut bytes_required: usize = 0;
        unsafe {
            InitializeProcThreadAttributeList(
                ptr::null_mut(),
                num_attributes,
                0,
                &mut bytes_required,
            )
        };
        let mut data = vec![0u8; bytes_required];

        let attr_ptr = data.as_mut_slice().as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        let res = unsafe {
            InitializeProcThreadAttributeList(attr_ptr, num_attributes, 0, &mut bytes_required)
        };
        if res == 0 {
            return Err(Error::other(format!(
                "InitializeProcThreadAttributeList failed: {}",
                IoError::last_os_error()
            )));
        }
        Ok(Self { data })
    }

    pub fn as_mut_ptr(&mut self) -> LPPROC_THREAD_ATTRIBUTE_LIST {
        self.data.as_mut_slice().as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST
    }

    pub fn set_pty(&mut self, con: HPCON) -> Result<()> {
        let res = unsafe {
            UpdateProcThreadAttribute(
                self.as_mut_ptr(),
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE,
                con as *const _,
                mem::size_of::<HPCON>(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if res == 0 {
            return Err(Error::other(format!(
                "UpdateProcThreadAttribute failed: {}",
                IoError::last_os_error()
            )));
        }
        Ok(())
    }
}

impl Drop for ProcThreadAttributeList {
    fn drop(&mut self) {
        unsafe { DeleteProcThreadAttributeList(self.as_mut_ptr()) };
    }
}
