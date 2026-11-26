//! RAM search (docs/games.md, "RAM search"): cheat-search style narrowing of candidate RAM
//! locations by how their values change, for finding and checking a game's RAM variables.
//! A developer tool: game RAM is never a policy's input.

use std::collections::HashSet;
use std::path::Path;

use gth_config::{Location, RamVar, Relation, SearchScript, Space, View};
use gth_libretro_sys::RETRO_MEMDESC_CONST;

use crate::{Core, Error, Result};

/// One searched region: its first offset (a core memory) or emulated address (the memory
/// map), and its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Region {
    start: usize,
    len: usize,
}

/// A candidate, packed: [`RamSearch`] starts with one per view and valid offset, which for a
/// large memory is millions.
#[derive(Debug, Clone, Copy)]
struct Slot {
    offset: u32,
    region: u16,
    view: u8,
}

/// A remaining candidate: where it is, how it is read and its value at the last snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    pub location: Location,
    pub view: View,
    pub value: i64,
}

impl Candidate {
    /// The RAM variable reading it (unscaled), as a `game.toml` `[ram]` entry would.
    pub fn ram_var(&self) -> RamVar {
        self.view.ram_var(self.location)
    }
}

/// A search over one [`Space`] through several [`View`]s at once. It keeps a snapshot of the
/// space and the candidates still consistent with every filter so far. It holds no borrow of
/// the [`Core`]: every call that reads memory takes the core, which must be the one (and the
/// game) the search started on; a different memory layout is an error.
pub struct RamSearch {
    space: Space,
    views: Vec<View>,
    /// Per view, the variable decoding it (location unused).
    decoders: Vec<RamVar>,
    regions: Vec<Region>,
    snapshot: Vec<Vec<u8>>,
    /// The next snapshot's buffers, kept to reuse their allocations.
    spare: Vec<Vec<u8>>,
    slots: Vec<Slot>,
}

fn search_error(reason: impl Into<String>) -> Error {
    Error::RamSearch {
        reason: reason.into(),
    }
}

/// The value of `decoder` at `offset` in `data`, or `None` if it doesn't fit or decode.
fn value(decoder: &RamVar, data: &[u8], offset: usize) -> Option<i64> {
    let bytes = data.get(offset..offset.checked_add(decoder.size)?)?;
    decoder.decode(bytes).ok()
}

/// The regions of `space` in `core`'s current game.
fn layout(core: &mut Core, space: Space) -> Result<Vec<Region>> {
    let regions: Vec<Region> = match space {
        Space::Memory(memory) => core
            .memory(memory.id())
            .map(|data| Region {
                start: 0,
                len: data.len(),
            })
            .into_iter()
            .collect(),
        Space::MemoryMap => {
            // Mirrors (the same host memory at several addresses) are searched once, at the
            // lowest address.
            let mut mapped: Vec<_> = (core.memory_regions().into_iter())
                .filter(|region| region.flags & RETRO_MEMDESC_CONST == 0 && region.len > 0)
                .collect();
            mapped.sort_by_key(|region| region.start);
            let mut seen = HashSet::new();
            (mapped.into_iter())
                .filter(|region| seen.insert((region.host_address, region.len)))
                .map(|region| Region {
                    start: region.start,
                    len: region.len,
                })
                .collect()
        }
    };
    if regions.is_empty() {
        let what = match space {
            Space::Memory(memory) => format!("no {}", memory.name()),
            Space::MemoryMap => "no writable memory map region".into(),
        };
        return Err(search_error(format!("the core exposes {what}")));
    }
    if u16::try_from(regions.len()).is_err()
        || regions
            .iter()
            .any(|region| u32::try_from(region.len).is_err())
    {
        return Err(search_error(
            "the memory has more than 65535 regions or one over 4 GiB",
        ));
    }
    Ok(regions)
}

/// Region `region` of `space`, or `None` if the core's memory no longer has it.
fn region_bytes(core: &mut Core, space: Space, region: Region) -> Option<&[u8]> {
    match space {
        Space::Memory(memory) => core
            .memory(memory.id())
            .filter(|data| data.len() == region.len),
        Space::MemoryMap => core.read_mapped(region.start, region.len),
    }
}

fn layout_changed(space: Space) -> Error {
    search_error(format!(
        "the core's {space} no longer has the layout the search started on (another game or core?)"
    ))
}

impl RamSearch {
    /// The most frames one [`watch`](Self::watch) runs: about half an hour at 60 fps.
    pub const MAX_WATCH_FRAMES: u32 = 108_000;

    /// The most values one [`watch`](Self::watch) returns (watched candidates times frames),
    /// about 160 MB of them.
    pub const MAX_WATCH_VALUES: u64 = 10_000_000;

    /// Runs `script` (docs/games.md, "RAM search") on `core` from its current state: starts a
    /// search of the script's space and views, then plays each step, loading its savestate
    /// from `base_dir`. Returns the search and the number of candidates at the start and after
    /// each step.
    pub fn run_script(
        core: &mut Core,
        script: &SearchScript,
        base_dir: &Path,
    ) -> Result<(RamSearch, Vec<usize>)> {
        let mut search = RamSearch::new(core, script.space, &script.views)?;
        let mut counts = Vec::with_capacity(script.steps.len() + 1);
        counts.push(search.len());
        for step in &script.steps {
            if let Some(state) = &step.state {
                let path = base_dir.join(state);
                let bytes = std::fs::read(&path).map_err(|error| {
                    search_error(format!(
                        "cannot read the savestate {}: {error}",
                        path.display()
                    ))
                })?;
                core.unserialize(&bytes)?;
            }
            if step.frames > 0 {
                core.run_frames(step.frames, step.buttons);
            }
            let current = search.capture(core)?;
            counts.push(search.narrow(current, step.relation));
        }
        Ok((search, counts))
    }

    /// Starts a search of `space` through `views`: snapshots it and makes every location
    /// where a view fits, and decodes, a candidate. `-swap16` views start only at even
    /// offsets (addresses, in the memory map). Fails if the core exposes no such memory or
    /// `views` is empty.
    pub fn new(core: &mut Core, space: Space, views: &[View]) -> Result<RamSearch> {
        if views.is_empty() || views.len() > usize::from(u8::MAX) + 1 {
            return Err(search_error("give 1 to 256 views"));
        }
        let regions = layout(core, space)?;
        let mut search = RamSearch::start(space, views, regions);
        search.snapshot = search.capture(core)?;
        search.slots = search.all_slots();
        Ok(search)
    }

    /// A search of `regions` with no snapshot and no candidates yet.
    fn start(space: Space, views: &[View], regions: Vec<Region>) -> RamSearch {
        RamSearch {
            space,
            views: views.to_vec(),
            decoders: (views.iter())
                .map(|view| view.ram_var(Location::Address(0)))
                .collect(),
            regions,
            snapshot: Vec::new(),
            spare: Vec::new(),
            slots: Vec::new(),
        }
    }

    /// Every slot that decodes in the current snapshot.
    fn all_slots(&self) -> Vec<Slot> {
        let mut slots = Vec::new();
        for (view, decoder) in self.decoders.iter().enumerate() {
            for (region, (bounds, data)) in self.regions.iter().zip(&self.snapshot).enumerate() {
                let Some(last) = bounds.len.checked_sub(decoder.size) else {
                    continue;
                };
                for offset in 0..=last {
                    if decoder.swap16 && !(bounds.start + offset).is_multiple_of(2) {
                        continue;
                    }
                    if value(decoder, data, offset).is_some() {
                        // The casts fit: `layout` checked the counts and lengths.
                        slots.push(Slot {
                            offset: offset as u32,
                            region: region as u16,
                            view: view as u8,
                        });
                    }
                }
            }
        }
        slots
    }

    /// A copy of every region, into the spare buffers.
    fn capture(&mut self, core: &mut Core) -> Result<Vec<Vec<u8>>> {
        let mut buffers = std::mem::take(&mut self.spare);
        buffers.resize_with(self.regions.len(), Vec::new);
        for (region, buffer) in self.regions.iter().zip(&mut buffers) {
            let data = region_bytes(core, self.space, *region)
                .ok_or_else(|| layout_changed(self.space))?;
            buffer.clear();
            buffer.extend_from_slice(data);
        }
        Ok(buffers)
    }

    /// Keeps the candidates whose value now relates to their value at the last snapshot by
    /// `relation` (a candidate that no longer decodes goes), takes a new snapshot and returns
    /// how many candidates are left.
    pub fn filter(&mut self, core: &mut Core, relation: Relation) -> Result<usize> {
        let current = self.capture(core)?;
        Ok(self.narrow(current, Some(relation)))
    }

    /// [`filter`](Self::filter), or [`rebase`](Self::rebase) without `relation`, with the new
    /// snapshot already taken.
    fn narrow(&mut self, current: Vec<Vec<u8>>, relation: Option<Relation>) -> usize {
        let previous = std::mem::replace(&mut self.snapshot, current);
        let (decoders, snapshot) = (&self.decoders, &self.snapshot);
        self.slots.retain(|slot| {
            let decoder = &decoders[usize::from(slot.view)];
            let (region, offset) = (usize::from(slot.region), slot.offset as usize);
            match (
                value(decoder, &previous[region], offset),
                value(decoder, &snapshot[region], offset),
            ) {
                (Some(old), Some(new)) => relation.is_none_or(|relation| relation.holds(old, new)),
                _ => false,
            }
        });
        self.spare = previous;
        self.slots.len()
    }

    /// Takes a new snapshot without comparing with the last one, so the next filter compares
    /// with now; drops only the candidates that no longer decode. Returns how many are left.
    pub fn rebase(&mut self, core: &mut Core) -> Result<usize> {
        let current = self.capture(core)?;
        Ok(self.narrow(current, None))
    }

    pub fn space(&self) -> Space {
        self.space
    }

    pub fn views(&self) -> &[View] {
        &self.views
    }

    /// Candidates left.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    fn location(&self, slot: Slot) -> Location {
        let at = self.regions[usize::from(slot.region)].start + slot.offset as usize;
        match self.space {
            Space::Memory(memory) => Location::Memory { memory, offset: at },
            Space::MemoryMap => Location::Address(at),
        }
    }

    /// The candidates left, by view (in the order given) then location, with their values at
    /// the last snapshot.
    pub fn candidates(&self) -> impl Iterator<Item = Candidate> + '_ {
        self.slots.iter().map(|&slot| {
            let decoder = &self.decoders[usize::from(slot.view)];
            let data = &self.snapshot[usize::from(slot.region)];
            Candidate {
                location: self.location(slot),
                view: self.views[usize::from(slot.view)],
                value: value(decoder, data, slot.offset as usize)
                    .expect("a candidate decodes in the snapshot it was kept in"),
            }
        })
    }

    /// Runs each `(buttons, frames)` input and returns, after every frame, the value of each of
    /// the first `limit` candidates (`None` where its bytes don't decode). Neither filters nor
    /// takes a snapshot; the frames run like any others, so the next filter sees them. At most
    /// [`MAX_WATCH_FRAMES`](Self::MAX_WATCH_FRAMES) frames and
    /// [`MAX_WATCH_VALUES`](Self::MAX_WATCH_VALUES) values in all, checked before any frame runs.
    pub fn watch(
        &self,
        core: &mut Core,
        inputs: &[(u16, u32)],
        limit: usize,
    ) -> Result<Vec<Vec<Option<i64>>>> {
        let watched = &self.slots[..limit.min(self.slots.len())];
        let frames: u64 = inputs.iter().map(|&(_, frames)| u64::from(frames)).sum();
        if frames > u64::from(Self::MAX_WATCH_FRAMES) {
            return Err(search_error(format!(
                "watch at most {} frames at once, not {frames}",
                Self::MAX_WATCH_FRAMES
            )));
        }
        let values = frames * watched.len() as u64;
        if values > Self::MAX_WATCH_VALUES {
            return Err(search_error(format!(
                "watch at most {} values (candidates times frames) at once, not {values}",
                Self::MAX_WATCH_VALUES
            )));
        }
        let mut rows = Vec::with_capacity(frames as usize);
        for &(buttons, frames) in inputs {
            for _ in 0..frames {
                core.run_frames(1, buttons);
                let mut row = vec![None; watched.len()];
                for (index, region) in self.regions.iter().enumerate() {
                    let data = region_bytes(core, self.space, *region)
                        .ok_or_else(|| layout_changed(self.space))?;
                    for (slot, cell) in watched.iter().zip(&mut row) {
                        if usize::from(slot.region) == index {
                            let decoder = &self.decoders[usize::from(slot.view)];
                            *cell = value(decoder, data, slot.offset as usize);
                        }
                    }
                }
                rows.push(row);
            }
        }
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gth_config::{Encoding, MemoryId};

    /// A search over plain buffers, as `new` would make it over a core's memory.
    fn search(space: Space, views: &[&str], regions: &[(usize, &[u8])]) -> RamSearch {
        let views: Vec<View> = views.iter().map(|view| view.parse().unwrap()).collect();
        let bounds = (regions.iter())
            .map(|&(start, data)| Region {
                start,
                len: data.len(),
            })
            .collect();
        let mut search = RamSearch::start(space, &views, bounds);
        search.snapshot = regions.iter().map(|(_, data)| data.to_vec()).collect();
        search.slots = search.all_slots();
        search
    }

    /// `filter` with the next snapshot given instead of read from a core.
    fn filter(search: &mut RamSearch, next: &[&[u8]], relation: Relation) -> usize {
        search.narrow(
            next.iter().map(|data| data.to_vec()).collect(),
            Some(relation),
        )
    }

    fn location_name(location: Location) -> String {
        match location {
            Location::Memory { memory, offset } => format!("{} {offset:#x}", memory.name()),
            Location::Address(address) => format!("address {address:#x}"),
        }
    }

    fn found(search: &RamSearch) -> Vec<(String, String, i64)> {
        search
            .candidates()
            .map(|c| (location_name(c.location), c.view.to_string(), c.value))
            .collect()
    }

    const RAM: Space = Space::Memory(MemoryId::SystemRam);

    /// `rebase` with the next snapshot given instead of read from a core.
    fn rebase(search: &mut RamSearch, next: &[&[u8]]) -> usize {
        search.narrow(next.iter().map(|data| data.to_vec()).collect(), None)
    }

    #[test]
    fn starts_with_every_location_that_decodes() {
        let data = [0x12, 0x3A, 0x00, 0x99, 0x01];
        let search = search(
            RAM,
            &["le1", "bcd1", "le2", "bcd2-swap16", "bcd4"],
            &[(0, &data)],
        );
        let count = |view: &str| {
            (search.candidates())
                .filter(|c| c.view.to_string() == view)
                .count()
        };
        assert_eq!(count("le1"), 5);
        assert_eq!(count("bcd1"), 4, "0x3a isn't BCD");
        assert_eq!(count("le2"), 4);
        // Even offsets only: [3a 12] no, [99 00] yes; offset 4 doesn't fit two bytes.
        assert_eq!(count("bcd2-swap16"), 1);
        assert_eq!(count("bcd4"), 0, "both four-byte windows hold 0x3a");
        assert_eq!(search.len(), 14);
    }

    #[test]
    fn swap16_parity_follows_the_address() {
        let data = [0u8; 6];
        let search = search(Space::MemoryMap, &["le2-swap16"], &[(0x101, &data)]);
        let addresses: Vec<String> = (search.candidates())
            .map(|c| location_name(c.location))
            .collect();
        assert_eq!(addresses, ["address 0x102", "address 0x104"]);
    }

    #[test]
    fn filters_by_each_relation() {
        let mut s = search(RAM, &["le1"], &[(0, &[1, 2, 3, 4])]);
        assert_eq!(filter(&mut s, &[&[1, 3, 2, 4]], Relation::Equal), 2);
        assert_eq!(
            found(&s),
            [
                ("system_ram 0x0".into(), "le1".into(), 1),
                ("system_ram 0x3".into(), "le1".into(), 4)
            ]
        );
        assert_eq!(filter(&mut s, &[&[2, 0, 0, 3]], Relation::Changed), 2);
        assert_eq!(filter(&mut s, &[&[5, 0, 0, 1]], Relation::Increased), 1);
        assert_eq!(found(&s), [("system_ram 0x0".into(), "le1".into(), 5)]);
        assert_eq!(filter(&mut s, &[&[4, 0, 0, 1]], Relation::Decreased), 1);
        assert_eq!(filter(&mut s, &[&[4, 0, 0, 1]], Relation::Equals(5)), 0);
    }

    #[test]
    fn drops_bcd_that_stops_decoding_and_reads_every_encoding() {
        let mut s = search(
            RAM,
            &["bcd2", "be2", "le2", "be4-swap16"],
            &[(0, &[0x12, 0x34, 0x56, 0x78])],
        );
        let values: Vec<_> = (s.candidates())
            .filter(|c| {
                c.location
                    == Location::Memory {
                        memory: MemoryId::SystemRam,
                        offset: 0,
                    }
            })
            .map(|c| (c.view.encoding, c.value))
            .collect();
        assert_eq!(
            values,
            [
                (Encoding::Bcd, 1234),
                (Encoding::Be, 0x1234),
                (Encoding::Le, 0x3412),
                (Encoding::Be, 0x3412_7856),
            ]
        );
        filter(&mut s, &[&[0x1A, 0x34, 0x56, 0x78]], Relation::Changed);
        assert_eq!(
            found(&s),
            [
                ("system_ram 0x0".into(), "be2".into(), 0x1A34),
                ("system_ram 0x0".into(), "le2".into(), 0x341A),
                ("system_ram 0x0".into(), "be4-swap16".into(), 0x341A_7856),
            ],
            "bcd2 at 0 stopped decoding"
        );
    }

    #[test]
    fn locations_are_offsets_or_addresses() {
        let s = search(
            Space::MemoryMap,
            &["le1"],
            &[(0x8000, &[7]), (0xF000, &[9])],
        );
        assert_eq!(
            found(&s),
            [
                ("address 0x8000".into(), "le1".into(), 7),
                ("address 0xf000".into(), "le1".into(), 9)
            ]
        );
        let candidate = s.candidates().next().unwrap();
        assert_eq!(candidate.ram_var().location, Location::Address(0x8000));
        assert_eq!(candidate.ram_var().scale, 1);
    }

    #[test]
    fn rebase_keeps_changed_values_but_drops_bcd_that_stops_decoding() {
        let mut search = search(Space::MemoryMap, &["bcd1", "le1"], &[(0, &[0x12, 0x34])]);
        assert_eq!(search.len(), 4);
        assert_eq!(rebase(&mut search, &[&[0x1A, 0x35]]), 3);
        assert_eq!(
            found(&search),
            [
                ("address 0x1", "bcd1", 35),
                ("address 0x0", "le1", 0x1A),
                ("address 0x1", "le1", 0x35)
            ]
            .map(|(at, view, value)| (at.to_string(), view.to_string(), value))
        );
        // The next filter compares with the rebased snapshot.
        assert_eq!(filter(&mut search, &[&[0x1A, 0x35]], Relation::Equal), 3);
    }
}
