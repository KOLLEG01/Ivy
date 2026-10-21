use super::*;

/// Owned binary SID; equality never resolves account names or assumes ownership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UserSid(Vec<u8>);

/// Captured effective identity, with a separate lazy user-SID read.
pub struct UserIdentity {
    token: OwnedHandle,
    user: Option<UserSid>,
}
impl UserIdentity {
    pub fn current() -> io::Result<Self> {
        let mut raw = null_mut();
        // WinSecurityContext.Both first tries OpenAsSelf, then the thread context.
        let opened = unsafe { OpenThreadToken(GetCurrentThread(), 0x02000000, 1, &mut raw) };
        if opened == 0 {
            raw = null_mut();
            if unsafe { OpenThreadToken(GetCurrentThread(), 0x02000000, 0, &mut raw) } == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(1008) {
                    return Err(error);
                }
                if unsafe { OpenProcessToken(GetCurrentProcess(), 0x02000000, &mut raw) } == 0 {
                    return Err(io::Error::last_os_error());
                }
            }
        }
        Ok(Self {
            token: owned(raw)?,
            user: None,
        })
    }
    pub fn from_token(token: BorrowedHandle<'_>) -> io::Result<Self> {
        let mut needed = 0;
        if unsafe { GetTokenInformation(token.as_raw_handle(), 8, null_mut(), 0, &mut needed) } == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(6) {
                return Err(error);
            }
        }
        let mut raw = null_mut();
        if unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                token.as_raw_handle(),
                GetCurrentProcess(),
                &mut raw,
                0,
                1,
                2,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            token: owned(raw)?,
            user: None,
        })
    }
    pub fn user_sid(&mut self) -> io::Result<Option<UserSid>> {
        if let Some(sid) = &self.user {
            return Ok(Some(sid.clone()));
        }
        let mut needed = 0;
        if unsafe { GetTokenInformation(self.token.as_raw_handle(), 1, null_mut(), 0, &mut needed) }
            == 0
        {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(122) {
                return Err(error);
            }
        }
        let mut data = vec![0usize; (needed as usize).div_ceil(size_of::<usize>())];
        if unsafe {
            GetTokenInformation(
                self.token.as_raw_handle(),
                1,
                data.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let base = data.as_ptr() as usize;
        let end = base + data.len() * size_of::<usize>();
        let pointer = *data
            .first()
            .ok_or_else(|| io::Error::other("Incomplete token user."))?;
        if pointer < base || pointer.checked_add(8).is_none_or(|value| value > end) {
            return Err(io::Error::other("Invalid token user SID."));
        }
        let prefix = unsafe { std::slice::from_raw_parts(pointer as *const u8, 8) };
        let length = 8 + usize::from(prefix[1]) * 4;
        if pointer.checked_add(length).is_none_or(|value| value > end) {
            return Err(io::Error::other("Incomplete token user SID."));
        }
        let sid =
            UserSid(unsafe { std::slice::from_raw_parts(pointer as *const u8, length) }.to_vec());
        self.user = Some(sid.clone());
        Ok(Some(sid))
    }
}
