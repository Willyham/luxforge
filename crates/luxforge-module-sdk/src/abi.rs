//! The `adjust` interface under the component model's canonical ABI, by hand, for a 32-bit linear
//! memory: what `export_adjustment!` exports. The host lowers each call's arguments into memory
//! the module allocates through [`realloc`], and lifts each answer from a return area this module
//! owns; each `post_*` function frees what its call answered once the host has copied it.
//!
//! Layouts, from `wit/adjustment.wit` under the canonical ABI:
//!
//! * `string` and `list<T>`: a pointer and a length, two `u32`s.
//! * `value`: a `u8` case at offset 0, its payload at offset 8 (the `f64` and `s64` cases align it
//!   to 8): 16 bytes, aligned to 8. A `choice` is a string at offset 8, a `color` three bytes.
//! * `field`: the name at offset 0, the value at offset 8: 24 bytes, aligned to 8.
//! * `result<list<u32>, string>` and `option<string>`: a `u8` case at offset 0, the list or string
//!   at offset 4: 12 bytes, aligned to 4.
//!
//! The arguments are the module's own once the call starts: each list and string is freed here
//! after it is read, with the layout the host allocated it with.
#![allow(unsafe_code)]

use crate::{Adjustment, Field, Value, Values};
use std::alloc::{Layout, alloc, dealloc};
use std::cell::UnsafeCell;

const FIELD_SIZE: usize = 24;
const FIELD_ALIGN: usize = 8;
const VALUE_AT: usize = 8;
const PAYLOAD_AT: usize = 8;

/// The return area every call answers through: one call runs at a time in a module instance, and
/// the host copies the answer out before the next.
#[repr(C, align(8))]
struct ReturnArea(UnsafeCell<[u8; 16]>);

// SAFETY: a WebAssembly module instance runs on one thread, and threads are off in the engine.
unsafe impl Sync for ReturnArea {}

static ANSWER: ReturnArea = ReturnArea(UnsafeCell::new([0; 16]));

fn answer() -> *mut u8 {
    ANSWER.0.get().cast()
}

/// `cabi_realloc`: a new allocation when `old_len` is zero, else `old`'s grown or shrunk. A
/// zero-sized allocation is the dangling pointer `align`, which nothing frees.
///
/// # Safety
///
/// `align` is a power of two, and `old`, when `old_len` is not zero, is a live allocation of
/// `old_len` bytes at `align` from this function.
pub unsafe fn realloc(old: *mut u8, old_len: usize, align: usize, new_len: usize) -> *mut u8 {
    let pointer = if old_len == 0 {
        if new_len == 0 {
            return align as *mut u8;
        }
        // SAFETY: the canonical ABI asks for a power-of-two alignment and a size it can address.
        unsafe { alloc(Layout::from_size_align_unchecked(new_len, align)) }
    } else {
        // SAFETY: the caller's contract: `old` was allocated here with this size and alignment.
        unsafe {
            std::alloc::realloc(
                old,
                Layout::from_size_align_unchecked(old_len, align),
                new_len,
            )
        }
    };
    if pointer.is_null() {
        std::process::abort();
    }
    pointer
}

/// Read a `u32` at `at`.
///
/// # Safety
///
/// `at` is four readable bytes.
unsafe fn read_u32(at: *const u8) -> u32 {
    // SAFETY: the caller's contract.
    unsafe { at.cast::<u32>().read_unaligned() }
}

/// Write a `u32` at `at`.
///
/// # Safety
///
/// `at` is four writable bytes.
unsafe fn write_u32(at: *mut u8, value: u32) {
    // SAFETY: the caller's contract.
    unsafe { at.cast::<u32>().write_unaligned(value) }
}

/// Take a lowered list of `T` the host allocated with `T`'s layout.
///
/// # Safety
///
/// `pointer` holds `len` initialized `T`s, allocated through [`realloc`] at `T`'s alignment, unless
/// `len` is zero.
unsafe fn take_list<T>(pointer: *mut u8, len: usize) -> Vec<T> {
    if len == 0 {
        return Vec::new();
    }
    // SAFETY: the caller's contract; `Layout::array::<T>(len)` is the layout the host asked for.
    unsafe { Vec::from_raw_parts(pointer.cast::<T>(), len, len) }
}

/// Take a lowered string.
///
/// # Safety
///
/// As [`take_list`] for bytes.
unsafe fn take_string(pointer: *mut u8, len: usize) -> String {
    // SAFETY: the caller's contract.
    let bytes = unsafe { take_list::<u8>(pointer, len) };
    String::from_utf8(bytes)
        .unwrap_or_else(|error| String::from_utf8_lossy(error.as_bytes()).into_owned())
}

/// Hand `items` to the host: its pointer and length, the allocation kept until the post-return
/// function takes it back with [`take_list`].
fn give_list<T>(items: Vec<T>) -> (u32, u32) {
    let items = items.into_boxed_slice();
    let len = items.len() as u32;
    (Box::into_raw(items).cast::<u8>() as u32, len)
}

/// Take the lowered `list<field>` at `pointer`.
///
/// # Safety
///
/// `pointer` holds `len` fields the host lowered, allocated through [`realloc`].
unsafe fn take_values(pointer: *mut u8, len: usize) -> Values {
    let mut fields = Vec::with_capacity(len);
    for at in 0..len {
        // SAFETY: field `at` is within the list.
        let field = unsafe { pointer.add(at * FIELD_SIZE) };
        // SAFETY: each read is within the field's 24 bytes, and each string the host lowered.
        unsafe {
            let name = take_string(read_u32(field) as *mut u8, read_u32(field.add(4)) as usize);
            let payload = field.add(VALUE_AT + PAYLOAD_AT);
            let value = match field.add(VALUE_AT).read() {
                0 => Value::Number(payload.cast::<f64>().read_unaligned()),
                1 => Value::Integer(payload.cast::<i64>().read_unaligned()),
                2 => Value::Boolean(payload.read() != 0),
                3 => Value::Choice(take_string(
                    read_u32(payload) as *mut u8,
                    read_u32(payload.add(4)) as usize,
                )),
                4 => Value::Color([payload.read(), payload.add(1).read(), payload.add(2).read()]),
                _ => std::process::abort(),
            };
            fields.push(Field { name, value });
        }
    }
    if len > 0 {
        // SAFETY: the host allocated the list with this layout through `realloc`.
        unsafe {
            dealloc(
                pointer,
                Layout::from_size_align_unchecked(len * FIELD_SIZE, FIELD_ALIGN),
            );
        }
    }
    Values::new(fields)
}

/// Write a case and a list's pointer and length into the return area.
fn answer_with(case: u8, (pointer, len): (u32, u32)) -> *mut u8 {
    let area = answer();
    // SAFETY: the return area is 16 bytes this module owns.
    unsafe {
        area.write(case);
        write_u32(area.add(4), pointer);
        write_u32(area.add(8), len);
    }
    area
}

/// `coefficients`: `result<list<u32>, string>`.
///
/// # Safety
///
/// Called by the host with a lowered `list<field>`.
pub unsafe fn coefficients<A: Adjustment>(values: *mut u8, len: usize) -> *mut u8 {
    // SAFETY: the caller's contract.
    let values = unsafe { take_values(values, len) };
    match A::coefficients(&values) {
        Ok(words) => answer_with(0, give_list(words)),
        Err(text) => answer_with(1, give_list(text.into_bytes())),
    }
}

/// Free what `coefficients` answered.
///
/// # Safety
///
/// `answer` is the return area `coefficients` answered, once.
pub unsafe fn post_coefficients(answer: *mut u8) {
    // SAFETY: the return area holds the case and the list `coefficients` gave.
    unsafe {
        let pointer = read_u32(answer.add(4)) as *mut u8;
        let len = read_u32(answer.add(8)) as usize;
        if answer.read() == 0 {
            drop(take_list::<u32>(pointer, len));
        } else {
            drop(take_list::<u8>(pointer, len));
        }
    }
}

/// `apply-row`: the row transformed in its own buffer, which the answer hands back.
///
/// # Safety
///
/// Called by the host with a lowered `list<u32>` and `list<f32>`.
pub unsafe fn apply_row<A: Adjustment>(
    words: *mut u8,
    words_len: usize,
    y: u32,
    x0: u32,
    rgb: *mut u8,
    rgb_len: usize,
) -> *mut u8 {
    // SAFETY: the caller's contract.
    let (words, mut rgb) = unsafe {
        (
            take_list::<u32>(words, words_len),
            take_list::<f32>(rgb, rgb_len),
        )
    };
    let (pixels, _) = rgb.as_chunks_mut::<3>();
    A::apply_row(&words, y, x0, pixels);
    let (pointer, len) = give_list(rgb);
    let area = answer();
    // SAFETY: the return area is 16 bytes this module owns.
    unsafe {
        write_u32(area, pointer);
        write_u32(area.add(4), len);
    }
    area
}

/// Free the row `apply-row` answered.
///
/// # Safety
///
/// `answer` is the return area `apply-row` answered, once.
pub unsafe fn post_apply_row(answer: *mut u8) {
    // SAFETY: the return area holds the list `apply_row` gave.
    unsafe {
        drop(take_list::<f32>(
            read_u32(answer) as *mut u8,
            read_u32(answer.add(4)) as usize,
        ));
    }
}

/// `label`: `option<string>`.
///
/// # Safety
///
/// Called by the host with a lowered `list<field>`.
pub unsafe fn label<A: Adjustment>(values: *mut u8, len: usize) -> *mut u8 {
    // SAFETY: the caller's contract.
    let values = unsafe { take_values(values, len) };
    match A::label(&values) {
        Some(text) => answer_with(1, give_list(text.into_bytes())),
        None => answer_with(0, (0, 0)),
    }
}

/// Free the text `label` answered.
///
/// # Safety
///
/// `answer` is the return area `label` answered, once.
pub unsafe fn post_label(answer: *mut u8) {
    // SAFETY: the return area holds the case and the string `label` gave.
    unsafe {
        if answer.read() == 1 {
            drop(take_list::<u8>(
                read_u32(answer.add(4)) as *mut u8,
                read_u32(answer.add(8)) as usize,
            ));
        }
    }
}
