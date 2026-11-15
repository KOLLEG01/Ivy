use super::*;

const TEXT_BYTES: usize = 24_000;
const ENTRIES: usize = 126;
const SEGMENT_IDS: usize = 512;

struct Entry {
    value: IvyJson,
    bytes: usize,
}

/// Retain the same recent suffix without copying or remeasuring older segments.
#[derive(Default)]
pub(super) struct Transcript {
    entries: VecDeque<Entry>,
    bytes: usize,
    segment_ids: VecDeque<JsString>,
}
impl Transcript {
    pub(super) fn from_items(items: &[IvyJson]) -> IvyResult<Self> {
        let mut result = Self::default();
        for item in items.iter().rev() {
            let bytes = text(item, "text")?.to_utf8()?.len();
            if bytes > TEXT_BYTES - result.bytes || result.entries.len() >= ENTRIES {
                break;
            }
            result.bytes += bytes;
            result.entries.push_front(Entry {
                value: item.clone(),
                bytes,
            });
        }
        Ok(result)
    }
    pub(super) fn push(&mut self, value: IvyJson) -> IvyResult<()> {
        let bytes = text(&value, "text")?.to_utf8()?.len();
        // An oversized latest entry ended the original reverse suffix scan.
        if bytes > TEXT_BYTES {
            self.entries.clear();
            self.bytes = 0;
            return Ok(());
        }
        while self.entries.len() >= ENTRIES || bytes > TEXT_BYTES - self.bytes {
            let removed = self
                .entries
                .pop_front()
                .expect("retained transcript prefix");
            self.bytes -= removed.bytes;
        }
        self.bytes += bytes;
        self.entries.push_back(Entry { value, bytes });
        Ok(())
    }
    pub(super) fn segment(&mut self, id: &JsString, value: IvyJson) -> IvyResult<()> {
        if self.segment_ids.contains(id) {
            return Ok(());
        }
        self.push(value)?;
        self.segment_ids.push_back(id.clone());
        if self.segment_ids.len() > SEGMENT_IDS {
            self.segment_ids.pop_front();
        }
        Ok(())
    }
    pub(super) fn snapshot(&self) -> Vec<IvyJson> {
        self.entries
            .iter()
            .map(|entry| entry.value.clone())
            .collect()
    }
    #[cfg(test)]
    pub(super) fn last(&self) -> Option<&IvyJson> {
        self.entries.back().map(|entry| &entry.value)
    }
}
