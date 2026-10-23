//! The original call dictionary enumerates occupied slots and reuses its last
//! freed position. Keep that observable selection order without a hash table.
pub(crate) struct CallSlots<T> {
    slots: Vec<Option<T>>,
    free: Vec<usize>,
    len: usize,
}
impl<T> Default for CallSlots<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
            len: 0,
        }
    }
}
impl<T> CallSlots<T> {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(Option::as_ref)
    }
    pub fn first(&self) -> Option<&T> {
        self.iter().next()
    }
    pub fn push(&mut self, value: T) {
        if let Some(slot) = self.free.pop() {
            self.slots[slot] = Some(value);
        } else {
            self.slots.push(Some(value));
        }
        self.len += 1;
    }
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.as_ref().is_some_and(|value| !keep(value)) {
                *slot = None;
                self.free.push(index);
                self.len -= 1;
            }
        }
    }
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free.clear();
        self.len = 0;
    }
}
