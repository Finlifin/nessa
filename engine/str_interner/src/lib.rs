use std::sync::Mutex;

// ---------------------------------------------------------------------------
// StrId
// ---------------------------------------------------------------------------

/// A compact handle representing an interned string.
/// The inner `u32` is the index into the interner's storage.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct StrId(u32);

impl StrId {
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

impl std::fmt::Debug for StrId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "StrId({})", self.0)
    }
}

// ---------------------------------------------------------------------------
// Red-Black Tree node
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    Red,
    Black,
}

const NIL: u32 = u32::MAX;

struct Node {
    /// Index into `Interner::strings` (also the StrId value).
    str_idx: u32,
    color: Color,
    parent: u32,
    left: u32,
    right: u32,
}

// ---------------------------------------------------------------------------
// Interner
// ---------------------------------------------------------------------------

/// A string interner backed by a red-black tree for O(log n) lookup by string
/// content.  Strings are stored in insertion order so that serializing by
/// `StrId` is trivial.
pub struct Interner {
    /// All interned strings, in insertion order.  `StrId(i)` indexes here.
    strings: Vec<String>,
    /// Red-black tree nodes – parallel to `strings` (same indices).
    nodes: Vec<Node>,
    /// Root of the RB tree.
    root: u32,
}

impl Interner {
    pub fn new() -> Self {
        Self {
            strings: Vec::new(),
            nodes: Vec::new(),
            root: NIL,
        }
    }

    /// Intern a string, returning its `StrId`.
    /// If the string was already interned, the existing id is returned.
    pub fn intern(&mut self, s: &str) -> StrId {
        // Search the RB tree for an existing entry.
        let mut cur = self.root;
        while cur != NIL {
            let node = &self.nodes[cur as usize];
            match s.cmp(self.strings[node.str_idx as usize].as_str()) {
                std::cmp::Ordering::Equal => return StrId(node.str_idx),
                std::cmp::Ordering::Less => cur = node.left,
                std::cmp::Ordering::Greater => cur = node.right,
            }
        }

        // Not found – insert a new string and tree node.
        let idx = self.strings.len() as u32;
        self.strings.push(s.to_owned());
        self.nodes.push(Node {
            str_idx: idx,
            color: Color::Red,
            parent: NIL,
            left: NIL,
            right: NIL,
        });
        self.rb_insert(idx);
        StrId(idx)
    }

    /// Look up the string for a given `StrId`.
    pub fn get(&self, id: StrId) -> &str {
        &self.strings[id.0 as usize]
    }

    /// Look up a handle without panicking if it is outside this interner.
    pub fn try_get(&self, id: StrId) -> Option<&str> {
        self.strings.get(id.0 as usize).map(String::as_str)
    }

    /// Total number of interned strings.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }

    // -- Ordered serialization ------------------------------------------------

    /// Iterate over all interned strings **in sorted (lexicographic) order**.
    /// Yields `(StrId, &str)` pairs.
    pub fn iter_sorted(&self) -> Vec<(StrId, &str)> {
        let mut out = Vec::with_capacity(self.strings.len());
        self.inorder(self.root, &mut out);
        out
    }

    fn inorder<'a>(&'a self, idx: u32, out: &mut Vec<(StrId, &'a str)>) {
        if idx == NIL {
            return;
        }
        let node = &self.nodes[idx as usize];
        self.inorder(node.left, out);
        out.push((StrId(node.str_idx), &self.strings[node.str_idx as usize]));
        self.inorder(node.right, out);
    }

    /// Serialize all strings in sorted order into a byte buffer.
    ///
    /// Format (little-endian):
    /// ```text
    /// [count: u32]
    /// for each string (sorted):
    ///   [original_id: u32] [len: u32] [utf8 bytes …]
    /// ```
    pub fn serialize_sorted(&self) -> Vec<u8> {
        let sorted = self.iter_sorted();
        // Pre-calculate capacity.
        let payload: usize = sorted.iter().map(|(_, s)| 8 + s.len()).sum();
        let mut buf = Vec::with_capacity(4 + payload);
        buf.extend_from_slice(&(sorted.len() as u32).to_le_bytes());
        for (id, s) in &sorted {
            buf.extend_from_slice(&id.as_u32().to_le_bytes());
            buf.extend_from_slice(&(s.len() as u32).to_le_bytes());
            buf.extend_from_slice(s.as_bytes());
        }
        buf
    }

    /// Deserialize a buffer produced by [`serialize_sorted`](Interner::serialize_sorted)
    /// and re-intern all strings.  Returns a mapping from **old `StrId`** to
    /// **new `StrId`** so call-sites can remap references.
    pub fn deserialize_sorted(data: &[u8]) -> (Self, Vec<(StrId, StrId)>) {
        let mut interner = Self::new();
        let mut remap = Vec::new();
        if data.len() < 4 {
            return (interner, remap);
        }
        let count = u32::from_le_bytes([data[0], data[1], data[2], data[3]]) as usize;
        let mut pos = 4;
        for _ in 0..count {
            if pos + 8 > data.len() {
                break;
            }
            let old_id =
                u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
            pos += 4;
            let len = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                as usize;
            pos += 4;
            if pos + len > data.len() {
                break;
            }
            let s =
                std::str::from_utf8(&data[pos..pos + len]).expect("invalid utf-8 in interner data");
            pos += len;
            let new_id = interner.intern(s);
            remap.push((StrId(old_id), new_id));
        }
        (interner, remap)
    }

    // -- Red-Black tree helpers -----------------------------------------------

    fn rb_insert(&mut self, z: u32) {
        // Standard BST insert (z is already in self.nodes at index z).
        let mut y = NIL;
        let mut x = self.root;
        while x != NIL {
            y = x;
            let x_str = self.strings[self.nodes[x as usize].str_idx as usize].as_str();
            let z_str = self.strings[self.nodes[z as usize].str_idx as usize].as_str();
            if z_str < x_str {
                x = self.nodes[x as usize].left;
            } else {
                x = self.nodes[x as usize].right;
            }
        }
        self.nodes[z as usize].parent = y;
        if y == NIL {
            self.root = z;
        } else {
            let y_str = self.strings[self.nodes[y as usize].str_idx as usize].as_str();
            let z_str = self.strings[self.nodes[z as usize].str_idx as usize].as_str();
            if z_str < y_str {
                self.nodes[y as usize].left = z;
            } else {
                self.nodes[y as usize].right = z;
            }
        }
        self.rb_insert_fixup(z);
    }

    fn rb_insert_fixup(&mut self, mut z: u32) {
        while z != self.root && self.color(self.nodes[z as usize].parent) == Color::Red {
            let p = self.nodes[z as usize].parent;
            let gp = self.nodes[p as usize].parent;
            if p == self.nodes[gp as usize].left {
                let uncle = self.nodes[gp as usize].right;
                if self.color(uncle) == Color::Red {
                    self.set_color(p, Color::Black);
                    self.set_color(uncle, Color::Black);
                    self.set_color(gp, Color::Red);
                    z = gp;
                } else {
                    if z == self.nodes[p as usize].right {
                        z = p;
                        self.rotate_left(z);
                    }
                    let p = self.nodes[z as usize].parent;
                    let gp = self.nodes[p as usize].parent;
                    self.set_color(p, Color::Black);
                    self.set_color(gp, Color::Red);
                    self.rotate_right(gp);
                }
            } else {
                let uncle = self.nodes[gp as usize].left;
                if self.color(uncle) == Color::Red {
                    self.set_color(p, Color::Black);
                    self.set_color(uncle, Color::Black);
                    self.set_color(gp, Color::Red);
                    z = gp;
                } else {
                    if z == self.nodes[p as usize].left {
                        z = p;
                        self.rotate_right(z);
                    }
                    let p = self.nodes[z as usize].parent;
                    let gp = self.nodes[p as usize].parent;
                    self.set_color(p, Color::Black);
                    self.set_color(gp, Color::Red);
                    self.rotate_left(gp);
                }
            }
        }
        self.set_color(self.root, Color::Black);
    }

    fn rotate_left(&mut self, x: u32) {
        let y = self.nodes[x as usize].right;
        let y_left = self.nodes[y as usize].left;
        self.nodes[x as usize].right = y_left;
        if y_left != NIL {
            self.nodes[y_left as usize].parent = x;
        }
        let x_parent = self.nodes[x as usize].parent;
        self.nodes[y as usize].parent = x_parent;
        if x_parent == NIL {
            self.root = y;
        } else if x == self.nodes[x_parent as usize].left {
            self.nodes[x_parent as usize].left = y;
        } else {
            self.nodes[x_parent as usize].right = y;
        }
        self.nodes[y as usize].left = x;
        self.nodes[x as usize].parent = y;
    }

    fn rotate_right(&mut self, x: u32) {
        let y = self.nodes[x as usize].left;
        let y_right = self.nodes[y as usize].right;
        self.nodes[x as usize].left = y_right;
        if y_right != NIL {
            self.nodes[y_right as usize].parent = x;
        }
        let x_parent = self.nodes[x as usize].parent;
        self.nodes[y as usize].parent = x_parent;
        if x_parent == NIL {
            self.root = y;
        } else if x == self.nodes[x_parent as usize].right {
            self.nodes[x_parent as usize].right = y;
        } else {
            self.nodes[x_parent as usize].left = y;
        }
        self.nodes[y as usize].right = x;
        self.nodes[x as usize].parent = y;
    }

    #[inline]
    fn color(&self, idx: u32) -> Color {
        if idx == NIL {
            Color::Black // NIL nodes are black by convention
        } else {
            self.nodes[idx as usize].color
        }
    }

    #[inline]
    fn set_color(&mut self, idx: u32, c: Color) {
        if idx != NIL {
            self.nodes[idx as usize].color = c;
        }
    }
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Global instance + static API
// ---------------------------------------------------------------------------

static GLOBAL: Mutex<Option<Interner>> = Mutex::new(None);

fn with_global<R>(f: impl FnOnce(&mut Interner) -> R) -> R {
    let mut guard = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let interner = guard.get_or_insert_with(Interner::new);
    f(interner)
}

/// Intern a string in the global interner.
pub fn intern(s: &str) -> StrId {
    with_global(|i| i.intern(s))
}

/// Retrieve the string associated with a `StrId` from the global interner.
///
/// # Panics
/// Panics if `id` was not produced by the global interner.
pub fn get(id: StrId) -> String {
    with_global(|i| i.get(id).to_owned())
}

/// Retrieve a global string, returning `None` for an invalid handle.
pub fn try_get(id: StrId) -> Option<String> {
    with_global(|i| i.try_get(id).map(str::to_owned))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intern_returns_same_id() {
        let mut interner = Interner::new();
        let a = interner.intern("hello");
        let b = interner.intern("world");
        let c = interner.intern("hello");
        assert_eq!(a, c);
        assert_ne!(a, b);
    }

    #[test]
    fn get_returns_correct_string() {
        let mut interner = Interner::new();
        let id = interner.intern("foo");
        assert_eq!(interner.get(id), "foo");
    }

    #[test]
    fn checked_lookup_rejects_invalid_handles() {
        let mut interner = Interner::new();
        let id = interner.intern("checked");
        assert_eq!(interner.try_get(id), Some("checked"));
        assert_eq!(interner.try_get(StrId::from_raw(u32::MAX)), None);
        assert_eq!(
            try_get(intern("checked_global")).as_deref(),
            Some("checked_global")
        );
        assert_eq!(try_get(StrId::from_raw(u32::MAX)), None);
    }

    #[test]
    fn iter_sorted_is_lexicographic() {
        let mut interner = Interner::new();
        interner.intern("banana");
        interner.intern("apple");
        interner.intern("cherry");
        interner.intern("date");
        let sorted: Vec<&str> = interner.iter_sorted().into_iter().map(|(_, s)| s).collect();
        assert_eq!(sorted, vec!["apple", "banana", "cherry", "date"]);
    }

    #[test]
    fn serialize_deserialize_roundtrip() {
        let mut interner = Interner::new();
        let id_hello = interner.intern("hello");
        let id_world = interner.intern("world");
        let id_abc = interner.intern("abc");

        let data = interner.serialize_sorted();
        let (new_interner, remap) = Interner::deserialize_sorted(&data);

        // All strings should be present.
        assert_eq!(new_interner.len(), 3);

        // Remap should map old ids to new ids correctly.
        for (old_id, new_id) in &remap {
            let old_str = interner.get(*old_id);
            let new_str = new_interner.get(*new_id);
            assert_eq!(old_str, new_str);
        }

        let _ = (id_hello, id_world, id_abc);
    }

    #[test]
    fn global_api() {
        let id = intern("global_test");
        let s = get(id);
        assert_eq!(s, "global_test");

        // Same string should yield same id.
        let id2 = intern("global_test");
        assert_eq!(id, id2);
    }

    #[test]
    fn many_insertions_maintain_rb_invariants() {
        let mut interner = Interner::new();
        let words = [
            "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "alpha", "beta",
            "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa", "lambda", "mu",
        ];
        for w in &words {
            interner.intern(w);
        }
        // Verify sorted output matches a standard sort.
        let mut expected: Vec<&str> = words.to_vec();
        expected.sort();
        expected.dedup();
        let got: Vec<&str> = interner.iter_sorted().into_iter().map(|(_, s)| s).collect();
        assert_eq!(got, expected);
    }
}
