use std::fmt;

/// Error type matching Go's nrzszError with type classification
#[derive(Debug, Clone)]
pub struct NrzszError {
    pub message: String,
    pub err_type: ErrorType,
    pub trace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorType {
    Fail,
    EXIT,
    None,
}

impl NrzszError {
    pub fn new(message: impl Into<String>, err_type: ErrorType, trace: bool) -> Self {
        Self {
            message: message.into(),
            err_type,
            trace,
        }
    }

    pub fn fail(message: impl Into<String>) -> Self {
        Self::new(message, ErrorType::Fail, false)
    }

    pub fn fail_trace(message: impl Into<String>) -> Self {
        Self::new(message, ErrorType::Fail, true)
    }

    pub fn exit(message: impl Into<String>) -> Self {
        Self::new(message, ErrorType::EXIT, false)
    }

    pub fn is_trace_back(&self) -> bool {
        self.trace
    }

    pub fn is_remote_exit(&self) -> bool {
        self.err_type == ErrorType::EXIT
    }

    pub fn is_remote_fail(&self) -> bool {
        self.err_type == ErrorType::Fail
    }
}

impl fmt::Display for NrzszError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for NrzszError {}

/// Check if an anyhow error is a remote exit
pub fn is_remote_exit(err: &anyhow::Error) -> bool {
    if let Some(e) = err.downcast_ref::<NrzszError>() {
        e.is_remote_exit()
    } else {
        false
    }
}

/// Check if an anyhow error is a remote fail
pub fn is_remote_fail(err: &anyhow::Error) -> bool {
    if let Some(e) = err.downcast_ref::<NrzszError>() {
        e.is_remote_fail()
    } else {
        false
    }
}

/// Check if an anyhow error is stop-and-delete
pub fn is_stop_and_delete(err: &anyhow::Error) -> bool {
    err.to_string().contains("stop and delete") || err.to_string().contains("stopped and delete")
}

/// Parse error type from line: #fail:, #FAIL:, #EXIT:
pub fn parse_error_type(line: &str) -> Option<(String, ErrorType)> {
    if let Some(idx) = line.find(':') {
        let prefix = &line[1..idx];
        let message = line[idx + 1..].trim().to_string();
        match prefix {
            "fail" | "FAIL" => Some((message, ErrorType::Fail)),
            "EXIT" => Some((message, ErrorType::EXIT)),
            _ => None,
        }
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nrzsz_error_fail() {
        let err = NrzszError::fail("something failed");
        assert!(err.is_remote_fail());
        assert!(!err.is_remote_exit());
        assert!(!err.is_trace_back());
    }

    #[test]
    fn test_nrzsz_error_exit() {
        let err = NrzszError::exit("remote exited");
        assert!(err.is_remote_exit());
        assert!(!err.is_remote_fail());
    }

    #[test]
    fn test_parse_error_type() {
        assert_eq!(parse_error_type("#fail:oops"), Some(("oops".into(), ErrorType::Fail)));
        assert_eq!(parse_error_type("#FAIL:oops"), Some(("oops".into(), ErrorType::Fail)));
        assert_eq!(parse_error_type("#EXIT:bye"), Some(("bye".into(), ErrorType::EXIT)));
        assert_eq!(parse_error_type("#DATA:123"), None);
    }
}
