//! Character-to-keycode cache builder.
//!
//! This module provides a way to translate characters to macOS virtual keycodes.
//! The translation depends on the current keyboard layout.
//!
//! The Carbon Text Input Source APIs must run on the main thread. Serializing worker calls
//! through a mutex does not satisfy their internal main-queue assertion. Build the cache while
//! creating the actor on the main thread, then move the actor to a worker for input/capture.

use std::collections::HashMap;
use std::ptr::NonNull;

use objc2::MainThreadMarker;
use objc2_core_foundation::{CFData, CFRetained, CFString, CFType};
use objc2_core_graphics::CGKeyCode;

// Carbon Text Input Services types and functions.
#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn TISCopyCurrentKeyboardInputSource() -> Option<NonNull<CFType>>;
    fn TISCopyCurrentKeyboardLayoutInputSource() -> Option<NonNull<CFType>>;
    fn TISGetInputSourceProperty(source: &CFType, key: &CFString) -> *mut CFType;

    // Property key for getting the keyboard layout data.
    static kTISPropertyUnicodeKeyLayoutData: &'static CFString;

    fn LMGetKbdType() -> u8;
}

// Unicode Utilities types and functions.
#[repr(C)]
#[allow(non_camel_case_types)]
struct UCKeyboardLayout {
    _opaque: [u8; 0],
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn UCKeyTranslate(
        layout: *const UCKeyboardLayout,
        virtual_key_code: u16,
        key_action: u16,
        modifier_key_state: u32,
        keyboard_type: u32,
        key_translate_options: u32,
        dead_key_state: *mut u32,
        max_string_length: usize,
        actual_string_length: *mut usize,
        unicode_string: *mut u16,
    ) -> i32;
}

// UCKeyTranslate constants.
const K_UC_KEY_ACTION_DOWN: u16 = 0;
const K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_BIT: u32 = 1 << 0;

// Shift modifier for UCKeyTranslate (shift bit position is 1 in the modifier state).
const SHIFT_MODIFIER: u32 = 1 << 1;

/// Builds a character-to-keycode cache for the current keyboard layout.
///
/// Callers must initialize the actor on the main thread. Refuse an accidental worker-thread
/// build before entering Carbon, which can abort the process rather than return an error. Do not
/// synchronously dispatch to the main queue: headless CLI callers may not service that queue.
/// TODO(QUALITY-271): Store the modifier keys as well.
pub fn build_cache() -> HashMap<char, CGKeyCode> {
    let Some(_main_thread) = MainThreadMarker::new() else {
        log::error!(
            "Computer-use actors must be initialized on the main thread; character keycode mapping is unavailable"
        );
        return HashMap::new();
    };
    let mut cache = HashMap::new();

    // Get the keyboard layout data.
    let layout_data = unsafe { get_keyboard_layout_data() };
    let Some(layout_data) = layout_data else {
        log::warn!("Failed to get keyboard layout data for keycode cache");
        return cache;
    };

    let layout_ptr = layout_data.byte_ptr() as *const UCKeyboardLayout;
    let keyboard_type = unsafe { LMGetKbdType() } as u32;

    // Iterate through all possible keycodes (0-127) and build the mapping.
    for keycode in 0u16..128 {
        // Get the character for this keycode without modifiers.
        if let Some(ch) = translate_keycode(layout_ptr, keycode, 0, keyboard_type)
            && !is_control_char(ch)
        {
            cache.entry(ch).or_insert(keycode as CGKeyCode);
        }

        // Get the character for this keycode with shift held.
        if let Some(ch) = translate_keycode(layout_ptr, keycode, SHIFT_MODIFIER, keyboard_type)
            && !is_control_char(ch)
        {
            cache.entry(ch).or_insert(keycode as CGKeyCode);
        }
    }

    cache
}

/// Gets the keyboard layout data from the current input source.
unsafe fn get_keyboard_layout_data() -> Option<CFRetained<CFData>> {
    // TISCopy* functions follow CF "Copy" semantics - caller owns the reference.
    // Wrap in CFRetained so they're released when dropped.
    let source = unsafe { CFRetained::from_raw(TISCopyCurrentKeyboardInputSource()?) };
    let mut layout_data =
        unsafe { TISGetInputSourceProperty(&source, kTISPropertyUnicodeKeyLayoutData) };

    // Some keyboard layouts (e.g., Japanese, Chinese) don't have layout data on the
    // regular input source. Try the keyboard layout input source instead.
    let _layout_source;
    if layout_data.is_null() {
        // Keep this alive until we're done with layout_data.
        _layout_source =
            unsafe { CFRetained::from_raw(TISCopyCurrentKeyboardLayoutInputSource()?) };
        layout_data =
            unsafe { TISGetInputSourceProperty(&_layout_source, kTISPropertyUnicodeKeyLayoutData) };
    }

    let layout_data = NonNull::new(layout_data)?;

    // The returned CFData is not retained, so we need to retain it.
    Some(unsafe { CFRetained::retain(layout_data.cast::<CFData>()) })
}

/// Translates a keycode to a character using UCKeyTranslate.
fn translate_keycode(
    layout: *const UCKeyboardLayout,
    keycode: u16,
    modifier_state: u32,
    keyboard_type: u32,
) -> Option<char> {
    let mut dead_key_state: u32 = 0;
    let mut string_length: usize = 0;
    let mut unicode_string = [0u16; 4];

    let result = unsafe {
        UCKeyTranslate(
            layout,
            keycode,
            K_UC_KEY_ACTION_DOWN,
            modifier_state,
            keyboard_type,
            K_UC_KEY_TRANSLATE_NO_DEAD_KEYS_BIT,
            &mut dead_key_state,
            unicode_string.len(),
            &mut string_length,
            unicode_string.as_mut_ptr(),
        )
    };

    if result != 0 || string_length == 0 {
        return None;
    }

    // Convert the first UTF-16 code unit to a char.
    // We only handle single-code-unit characters for simplicity.
    char::decode_utf16(unicode_string[..string_length].iter().copied())
        .next()
        .and_then(|r| r.ok())
}

/// Returns true if the character is a control character (non-printable).
fn is_control_char(ch: char) -> bool {
    // C0 control characters (0x00-0x1F) and C1 control characters (0x7F-0x9F)
    let code = ch as u32;
    code <= 0x1F || (0x7F..=0x9F).contains(&code)
}
