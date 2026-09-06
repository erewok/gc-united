//! The raw heap: a flat byte arena addressed by 32-bit handles.
//!
//! Every object is laid out as a fixed 16-byte header followed by `nrefs`
//! 32-bit reference slots and then `ndata` bytes of scalar payload. The whole
//! object is padded up to an 8-byte boundary.
//!
//! ```text
//!  0      4     6     7     8         12        16          16+4*nrefs
//!  +------+-----+-----+-----+---------+---------+-----------+---------+
//!  | size |nrefs|flags|color|   fwd   |   rc    | ref slots |  data   |
//!  +------+-----+-----+-----+---------+---------+-----------+---------+
//! ```
//!
//! `Heap` deliberately knows nothing about garbage collection. It cannot
//! allocate: it only places objects where a collector tells it to, and copies
//! them where a collector tells it to. Allocation policy, reclamation and
//! space management all belong to the collector.

use std::fmt;

/// Bytes of header in front of every object.
pub const HEADER_SIZE: u32 = 16;
/// Every object begins on a multiple of this many bytes.
///
/// This equals [`HEADER_SIZE`] on purpose. Because every object size is a
/// multiple of the alignment, splitting a free block can never leave a
/// remainder too small to carry a header of its own, so an allocator never has
/// to round a request up and account for bytes it did not hand out.
pub const ALIGN: u32 = 16;
/// Address reserved to mean "no object".
pub const NULL_ADDR: u32 = u32::MAX;

const OFF_SIZE: u32 = 0;
const OFF_NREFS: u32 = 4;
const OFF_FLAGS: u32 = 6;
const OFF_COLOR: u32 = 7;
const OFF_FWD: u32 = 8;
const OFF_RC: u32 = 12;

/// Set while an object is reachable during a tracing collection.
pub const FLAG_MARK: u8 = 0b0000_0001;
/// Set on objects that must not be moved by a relocating collector.
pub const FLAG_PINNED: u8 = 0b0000_0010;
/// General-purpose "already in some worklist" bit.
pub const FLAG_BUFFERED: u8 = 0b0000_0100;
/// Set on blocks that are on a free list rather than holding a live object.
pub const FLAG_FREE: u8 = 0b0000_1000;

const GEN_SHIFT: u8 = 4;
const GEN_MASK: u8 = 0b0011_0000;
const AGE_SHIFT: u8 = 6;
const AGE_MASK: u8 = 0b1100_0000;

/// Tri-colour marking states. See module 8.
pub const WHITE: u8 = 0;
pub const GREY: u8 = 1;
pub const BLACK: u8 = 2;

/// Byte pattern written over the payload of a reclaimed object.
pub const POISON: u8 = 0xDE;

/// A reference to a heap object: a byte offset into the arena.
///
/// Handles are *not* stable across a relocating collection. Code that must
/// survive a collection keeps a [`crate::roots::RootSlot`] instead.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Handle(pub u32);

/// The null handle.
pub const NULL: Handle = Handle(NULL_ADDR);

impl Handle {
    #[inline]
    pub fn is_null(self) -> bool {
        self.0 == NULL_ADDR
    }
    #[inline]
    pub fn addr(self) -> u32 {
        self.0
    }
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_null() {
            write!(f, "null")
        } else {
            write!(f, "@{:#x}", self.0)
        }
    }
}

/// Round `n` up to the next multiple of [`ALIGN`].
#[inline]
pub fn align_up(n: u32) -> u32 {
    (n + ALIGN - 1) & !(ALIGN - 1)
}

/// A flat byte arena.
pub struct Heap {
    bytes: Vec<u8>,
}

impl Heap {
    /// Create an arena of exactly `capacity` bytes, zero-filled.
    pub fn new(capacity: usize) -> Heap {
        assert!(capacity >= HEADER_SIZE as usize, "heap too small");
        assert!(
            capacity < NULL_ADDR as usize,
            "heap must be addressable by u32"
        );
        assert!(
            capacity.is_multiple_of(ALIGN as usize),
            "heap capacity must be a multiple of {ALIGN} bytes"
        );
        Heap {
            bytes: vec![0u8; capacity],
        }
    }

    /// Total bytes in the arena.
    #[inline]
    pub fn capacity(&self) -> u32 {
        self.bytes.len() as u32
    }

    /// Total footprint, in bytes, of an object with this shape.
    #[inline]
    pub fn size_for(nrefs: u16, ndata: u32) -> u32 {
        align_up(HEADER_SIZE + 4 * nrefs as u32 + ndata)
    }

    /// Write a fresh object header at `at` and null out its reference slots.
    ///
    /// This is placement only: the caller has already decided that the bytes
    /// at `at` are free.
    pub fn emplace(&mut self, at: u32, nrefs: u16, ndata: u32) -> Handle {
        let size = Self::size_for(nrefs, ndata);
        assert!(
            at.is_multiple_of(ALIGN),
            "emplace at unaligned address {at:#x}"
        );
        assert!(
            at.saturating_add(size) <= self.capacity(),
            "emplace of {size} bytes at {at:#x} runs past the end of a {} byte heap",
            self.capacity()
        );
        let (s, e) = (at as usize, (at + size) as usize);
        self.bytes[s..e].fill(0);
        self.set_u32(at + OFF_SIZE, size);
        self.set_u16(at + OFF_NREFS, nrefs);
        self.bytes[(at + OFF_FLAGS) as usize] = 0;
        self.bytes[(at + OFF_COLOR) as usize] = WHITE;
        self.set_u32(at + OFF_FWD, NULL_ADDR);
        self.set_u32(at + OFF_RC, 0);
        for i in 0..nrefs {
            self.set_u32(at + HEADER_SIZE + 4 * i as u32, NULL_ADDR);
        }
        Handle(at)
    }

    /// Write a header describing a free block of exactly `size` bytes.
    ///
    /// A linear sweep of the heap depends on every block carrying its true
    /// size, free blocks included; this is how a hole announces how far the
    /// next object is.
    pub fn emplace_block(&mut self, at: u32, size: u32) -> Handle {
        assert!(
            at.is_multiple_of(ALIGN),
            "block at unaligned address {at:#x}"
        );
        assert!(
            size >= HEADER_SIZE && size.is_multiple_of(ALIGN),
            "bad block size {size}"
        );
        assert!(
            at.saturating_add(size) <= self.capacity(),
            "block of {size} bytes at {at:#x} runs past the end of the heap"
        );
        self.set_u32(at + OFF_SIZE, size);
        self.set_u16(at + OFF_NREFS, 0);
        self.bytes[(at + OFF_FLAGS) as usize] = FLAG_FREE;
        self.bytes[(at + OFF_COLOR) as usize] = WHITE;
        self.set_u32(at + OFF_FWD, NULL_ADDR);
        self.set_u32(at + OFF_RC, 0);
        Handle(at)
    }

    /// Copy an object's bytes to `to`, returning a handle to the copy.
    ///
    /// Source and destination ranges may overlap, which is the normal case for
    /// a sliding compactor.
    pub fn relocate(&mut self, from: Handle, to: u32) -> Handle {
        let size = self.size(from) as usize;
        assert!(
            to.is_multiple_of(ALIGN),
            "relocate to unaligned address {to:#x}"
        );
        assert!(
            to as usize + size <= self.bytes.len(),
            "relocate of {size} bytes to {to:#x} runs past the end of the heap"
        );
        self.bytes
            .copy_within(from.0 as usize..from.0 as usize + size, to as usize);
        Handle(to)
    }

    /// Overwrite an object's payload with [`POISON`] and flag it free.
    ///
    /// The header is left intact so that a linear sweep of the heap can still
    /// find the following object.
    pub fn poison(&mut self, h: Handle) {
        let start = h.0 + HEADER_SIZE;
        let end = h.0 + self.size(h);
        self.bytes[start as usize..end as usize].fill(POISON);
        self.set_flag(h, FLAG_FREE, true);
    }

    /// True if the payload of `h` is entirely [`POISON`].
    pub fn is_poisoned(&self, h: Handle) -> bool {
        let start = (h.0 + HEADER_SIZE) as usize;
        let end = (h.0 + self.size(h)) as usize;
        end > start && self.bytes[start..end].iter().all(|&b| b == POISON)
    }

    // ---- header accessors -------------------------------------------------

    /// Total size of the object in bytes, header included.
    #[inline]
    pub fn size(&self, h: Handle) -> u32 {
        self.u32(h.0 + OFF_SIZE)
    }
    #[inline]
    pub fn set_size(&mut self, h: Handle, size: u32) {
        self.set_u32(h.0 + OFF_SIZE, size);
    }
    /// Number of reference slots in the object.
    #[inline]
    pub fn nrefs(&self, h: Handle) -> u16 {
        self.u16(h.0 + OFF_NREFS)
    }
    #[inline]
    pub fn set_nrefs(&mut self, h: Handle, n: u16) {
        self.set_u16(h.0 + OFF_NREFS, n);
    }
    #[inline]
    pub fn flags(&self, h: Handle) -> u8 {
        self.bytes[(h.0 + OFF_FLAGS) as usize]
    }
    #[inline]
    pub fn set_flags(&mut self, h: Handle, f: u8) {
        self.bytes[(h.0 + OFF_FLAGS) as usize] = f;
    }
    #[inline]
    pub fn flag(&self, h: Handle, bit: u8) -> bool {
        self.flags(h) & bit != 0
    }
    #[inline]
    pub fn set_flag(&mut self, h: Handle, bit: u8, on: bool) {
        let f = self.flags(h);
        self.set_flags(h, if on { f | bit } else { f & !bit });
    }
    #[inline]
    pub fn is_marked(&self, h: Handle) -> bool {
        self.flag(h, FLAG_MARK)
    }
    #[inline]
    pub fn set_marked(&mut self, h: Handle, on: bool) {
        self.set_flag(h, FLAG_MARK, on);
    }
    /// Tri-colour state: [`WHITE`], [`GREY`] or [`BLACK`].
    #[inline]
    pub fn color(&self, h: Handle) -> u8 {
        self.bytes[(h.0 + OFF_COLOR) as usize]
    }
    #[inline]
    pub fn set_color(&mut self, h: Handle, c: u8) {
        self.bytes[(h.0 + OFF_COLOR) as usize] = c;
    }
    /// Generation number, 0..=3.
    #[inline]
    pub fn generation(&self, h: Handle) -> u8 {
        (self.flags(h) & GEN_MASK) >> GEN_SHIFT
    }
    #[inline]
    pub fn set_generation(&mut self, h: Handle, g: u8) {
        let f = (self.flags(h) & !GEN_MASK) | ((g << GEN_SHIFT) & GEN_MASK);
        self.set_flags(h, f);
    }
    /// Survival count, 0..=3, used to decide promotion.
    #[inline]
    pub fn age(&self, h: Handle) -> u8 {
        (self.flags(h) & AGE_MASK) >> AGE_SHIFT
    }
    #[inline]
    pub fn set_age(&mut self, h: Handle, a: u8) {
        let f = (self.flags(h) & !AGE_MASK) | ((a << AGE_SHIFT) & AGE_MASK);
        self.set_flags(h, f);
    }
    /// Forwarding address left behind by a relocating collector, if any.
    #[inline]
    pub fn forward(&self, h: Handle) -> Handle {
        Handle(self.u32(h.0 + OFF_FWD))
    }
    #[inline]
    pub fn set_forward(&mut self, h: Handle, to: Handle) {
        self.set_u32(h.0 + OFF_FWD, to.0);
    }
    /// Reference count, used by modules 2 and 3.
    #[inline]
    pub fn rc(&self, h: Handle) -> u32 {
        self.u32(h.0 + OFF_RC)
    }
    #[inline]
    pub fn set_rc(&mut self, h: Handle, rc: u32) {
        self.set_u32(h.0 + OFF_RC, rc);
    }

    // ---- payload accessors ------------------------------------------------

    /// Read reference slot `i`.
    #[inline]
    pub fn field(&self, h: Handle, i: u16) -> Handle {
        debug_assert!(i < self.nrefs(h), "field {i} out of range for {h:?}");
        Handle(self.u32(h.0 + HEADER_SIZE + 4 * i as u32))
    }
    /// Write reference slot `i`. This is a raw store with no write barrier.
    #[inline]
    pub fn set_field(&mut self, h: Handle, i: u16, v: Handle) {
        debug_assert!(i < self.nrefs(h), "field {i} out of range for {h:?}");
        self.set_u32(h.0 + HEADER_SIZE + 4 * i as u32, v.0);
    }
    /// Every non-null reference slot of `h`.
    pub fn children(&self, h: Handle) -> Vec<Handle> {
        (0..self.nrefs(h))
            .map(|i| self.field(h, i))
            .filter(|c| !c.is_null())
            .collect()
    }

    /// Byte offset at which the scalar payload of `h` starts.
    #[inline]
    pub fn data_offset(&self, h: Handle) -> u32 {
        h.0 + HEADER_SIZE + 4 * self.nrefs(h) as u32
    }
    /// The scalar payload of `h`, including any trailing alignment padding.
    #[inline]
    pub fn data(&self, h: Handle) -> &[u8] {
        let s = self.data_offset(h) as usize;
        let e = (h.0 + self.size(h)) as usize;
        &self.bytes[s..e]
    }
    #[inline]
    pub fn data_mut(&mut self, h: Handle) -> &mut [u8] {
        let s = self.data_offset(h) as usize;
        let e = (h.0 + self.size(h)) as usize;
        &mut self.bytes[s..e]
    }
    /// Read 8 payload bytes at `off` as a little-endian `u64`.
    pub fn data_u64(&self, h: Handle, off: u32) -> u64 {
        let d = self.data(h);
        let o = off as usize;
        u64::from_le_bytes(d[o..o + 8].try_into().unwrap())
    }
    pub fn set_data_u64(&mut self, h: Handle, off: u32, v: u64) {
        let d = self.data_mut(h);
        let o = off as usize;
        d[o..o + 8].copy_from_slice(&v.to_le_bytes());
    }
    pub fn data_u32(&self, h: Handle, off: u32) -> u32 {
        let d = self.data(h);
        let o = off as usize;
        u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
    }
    pub fn set_data_u32(&mut self, h: Handle, off: u32, v: u32) {
        let d = self.data_mut(h);
        let o = off as usize;
        d[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    // ---- linear traversal -------------------------------------------------

    /// Walk objects laid out contiguously in `[from, to)`, in address order.
    ///
    /// Every object in the range must carry a valid size in its header; a zero
    /// size would not advance and is reported as an error rather than hanging.
    pub fn walk(&self, from: u32, to: u32) -> Walk<'_> {
        Walk {
            heap: self,
            at: from,
            end: to,
        }
    }

    /// True if `h` addresses a plausibly-shaped object inside the arena.
    pub fn in_bounds(&self, h: Handle) -> bool {
        if h.is_null() || !h.0.is_multiple_of(ALIGN) || h.0 + HEADER_SIZE > self.capacity() {
            return false;
        }
        let size = self.u32(h.0 + OFF_SIZE);
        size >= HEADER_SIZE && size.is_multiple_of(ALIGN) && h.0 + size <= self.capacity()
    }

    // ---- raw little-endian access ----------------------------------------

    #[inline]
    fn u16(&self, off: u32) -> u16 {
        let o = off as usize;
        u16::from_le_bytes(self.bytes[o..o + 2].try_into().unwrap())
    }
    #[inline]
    fn set_u16(&mut self, off: u32, v: u16) {
        let o = off as usize;
        self.bytes[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }
    #[inline]
    fn u32(&self, off: u32) -> u32 {
        let o = off as usize;
        u32::from_le_bytes(self.bytes[o..o + 4].try_into().unwrap())
    }
    #[inline]
    fn set_u32(&mut self, off: u32, v: u32) {
        let o = off as usize;
        self.bytes[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    /// Raw arena bytes, for tests and heap dumps.
    pub fn raw(&self) -> &[u8] {
        &self.bytes
    }
}

/// Iterator over contiguously laid out objects. See [`Heap::walk`].
pub struct Walk<'h> {
    heap: &'h Heap,
    at: u32,
    end: u32,
}

impl Iterator for Walk<'_> {
    type Item = Handle;
    fn next(&mut self) -> Option<Handle> {
        if self.at + HEADER_SIZE > self.end {
            return None;
        }
        let h = Handle(self.at);
        let size = self.heap.size(h);
        assert!(
            size >= HEADER_SIZE && size.is_multiple_of(ALIGN),
            "walking the heap hit a block at {:#x} whose header claims size {size}; \
             a linear walk needs every block, live or free, to carry its true size",
            self.at
        );
        self.at += size;
        Some(h)
    }
}
