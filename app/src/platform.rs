//! What differs by operating system, behind one face: a notification.

/// Show a notification. Best effort: a platform that refuses is not a
/// reason to stop watching.
#[cfg_attr(test, allow(dead_code))]
pub fn notify(title: &str, body: &str) {
    let _ = notify_rust::Notification::new().summary(title).body(body).show();
}
