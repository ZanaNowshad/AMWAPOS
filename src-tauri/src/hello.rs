//! Windows Hello step-up (module `windows_hello`). Called by the runtime for
//! sensitive commands after the staff PIN / manager approval; it never
//! replaces the PIN.

use amwapos_hub::runtime::StepUp;

#[cfg(windows)]
pub fn verify(message: &str) -> StepUp {
    use windows::core::HSTRING;
    use windows::Security::Credentials::UI::{UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability};

    let availability = match UserConsentVerifier::CheckAvailabilityAsync().and_then(|op| op.get()) {
        Ok(a) => a,
        Err(e) => return StepUp::Unavailable(format!("Windows Hello availability check failed: {e}")),
    };
    if availability != UserConsentVerifierAvailability::Available {
        return StepUp::Unavailable(format!("Windows Hello is not available ({availability:?})."));
    }
    match UserConsentVerifier::RequestVerificationAsync(&HSTRING::from(message)).and_then(|op| op.get()) {
        Ok(UserConsentVerificationResult::Verified) => StepUp::Verified,
        Ok(other) => StepUp::Refused(format!("{other:?}")),
        Err(e) => StepUp::Refused(e.to_string()),
    }
}

#[cfg(not(windows))]
pub fn verify(_message: &str) -> StepUp {
    StepUp::Unavailable("Windows Hello exists only on Windows.".into())
}
