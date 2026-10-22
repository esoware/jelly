use raw_window_handle::HandleError;
use windows::core::Error as WindowsError;

#[derive(Debug, thiserror::Error)]
pub enum Win32Error {
    #[error("the window has no native handle")]
    Handle(#[from] HandleError),
    #[error("the window is not a Win32 window")]
    NotWin32,
    #[error("a Windows call failed with {}", .0.code())]
    Call(#[from] WindowsError),
}
