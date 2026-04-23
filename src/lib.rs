pub mod raw;
pub use crate::raw::RawHandovers;

use std::{
    collections::{HashMap, HashSet},
    fs::{read, write},
    path::{Path, PathBuf},
    str::FromStr,
    u32,
};

use clap::Parser;
use serde::{Deserialize, Serialize};
use tracing::{debug, debug_span, error, info, info_span, warn};

use constellation::constellation::{ConnectivityEventType, ConnectivityEventsDump};

pub type Timestamp = u64;
pub type GatewayName = String;

#[derive(Deserialize, Serialize, Debug, Hash, Eq, PartialEq, Ord, PartialOrd, Clone)]
pub struct SatPos {
    pub plane: u32,
    pub rank: u32,
}

impl Into<(usize, usize)> for &SatPos {
    fn into(self) -> (usize, usize) {
        (self.plane as usize, self.rank as usize)
    }
}

impl From<(u32, u32)> for SatPos {
    fn from(value: (u32, u32)) -> Self {
        Self {
            plane: value.0,
            rank: value.1,
        }
    }
}

#[derive(Deserialize, Serialize, Debug, PartialEq, PartialOrd, Ord, Eq, Hash, Clone)]
pub enum HandoverType {
    Addition,
    Removal,
}

impl FromStr for HandoverType {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "del" => Ok(HandoverType::Removal),
            "add" => Ok(HandoverType::Addition),
            _ => Err(()),
        }
    }
}

impl TryFrom<String> for HandoverType {
    type Error = ();
    fn try_from(value: String) -> Result<Self, Self::Error> {
        HandoverType::from_str(&value)
    }
}

#[derive(Deserialize, Serialize, Debug)]
pub struct Handover {
    pub sat: SatPos,
    pub distance: Option<f64>,
}

impl From<((u32, u32), Option<f64>)> for Handover {
    fn from(value: ((u32, u32), Option<f64>)) -> Self {
        Self {
            sat: SatPos::from(value.0),
            distance: value.1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct ConnectionData {
    distance: Distance,
    ts_lost_of_sight: Timestamp,
}

pub type GatewayEvents = HashMap<Timestamp, HashMap<GatewayName, Vec<Handover>>>;
pub type Distance = Option<f64>;
type SatConnectedPerGw<'a> = HashMap<&'a GatewayName, HashSet<&'a SatPos>>;
type GwConnectedPerSat<'a> = HashMap<&'a SatPos, HashSet<&'a GatewayName>>;
type GwCandidatesPerSat<'a> = HashMap<&'a SatPos, HashMap<&'a GatewayName, ConnectionData>>;
type SatCandidatesPerGw<'a> = HashMap<&'a GatewayName, HashMap<&'a SatPos, ConnectionData>>;
type ConnectivityEvents<'a> = HashMap<&'a GatewayName, Vec<(Timestamp, HandoverType, &'a SatPos)>>;
type SatHistory<'a> = HashMap<Timestamp, Vec<(&'a GatewayName, HandoverType)>>;
type FilteredEvents<'a> = HashMap<&'a GatewayName, Vec<(Timestamp, EmitHandover<'a>)>>;
type GwStats<'a> = HashMap<&'a GatewayName, Stats>;

pub const GW_MAX_CO: usize = 9;
pub const SAT_MAX_CO: usize = 4;

#[derive(Deserialize, Clone, Debug)]
pub struct MaxGwPerSat(usize);
impl Default for MaxGwPerSat {
    fn default() -> Self {
        Self(SAT_MAX_CO)
    }
}
impl FromStr for MaxGwPerSat {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(usize::from_str(s)?))
    }
}

#[derive(Deserialize, Clone, Debug)]
pub struct MaxSatPerGw(usize);
impl Default for MaxSatPerGw {
    fn default() -> Self {
        Self(GW_MAX_CO)
    }
}
impl FromStr for MaxSatPerGw {
    type Err = std::num::ParseIntError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(usize::from_str(s)?))
    }
}

#[derive(Parser, Deserialize, Debug)]
pub struct Parameters {
    #[arg(short, long)]
    pub file: PathBuf,
    #[arg(long)]
    #[serde(default)]
    max_gw_per_sat: MaxGwPerSat,
    #[arg(long)]
    #[serde(default)]
    max_sat_per_gw: MaxSatPerGw,
    #[arg(short, long, default_value_t = 60)]
    pub max_delay: u64,
    #[arg(short)]
    #[serde(default)]
    skip: Vec<GatewayName>,
    #[arg(long)]
    #[serde(default)]
    new_format: bool,
}

pub fn load_new(content: Vec<u8>) -> Result<RawHandovers, ()> {
    let dump = serde_json::from_slice::<ConnectivityEventsDump>(&content).map_err(|_| ())?;
    let events = dump.events;

    let mut initial: HashMap<String, Vec<SatPos>> = Default::default();
    let mut history: HashMap<String, HashMap<u64, HashMap<String, Vec<Handover>>>> =
        Default::default();

    events.iter().for_each(|(gw, events)| {
        events.iter().for_each(|event| {
            let sat = SatPos {
                plane: event.sat.0 as u32,
                rank: event.sat.1 as u32,
            };
            if event.ts == 0f64 {
                initial.entry(gw.clone()).or_default().push(sat);
            } else {
                let co_event = Handover {
                    sat,
                    distance: Some(event.distance),
                };
                history
                    .entry(gw.clone())
                    .or_default()
                    .entry(event.ts as u64)
                    .or_default()
                    .entry(
                        if matches!(event.event, ConnectivityEventType::Addition) {
                            "add"
                        } else {
                            "del"
                        }
                        .to_string(),
                    )
                    .or_default()
                    .push(co_event);
            }
        });
    });

    Ok(RawHandovers {
        initial,
        history,
        period: dump.period,
    })
}

pub fn load(cfg: &Parameters) -> Result<RawHandovers, ()> {
    info!("Loading connectivity events...");
    let content = read(&cfg.file).map_err(|_| ())?;
    let events = if cfg.new_format {
        load_new(content)
    } else {
        serde_json::from_slice::<RawHandovers>(&content).map_err(|_| ())
    };
    // debug!(?events);
    events
}

pub fn run<'a>(raw: &'a RawHandovers, cfg: &Parameters, dump: Option<PathBuf>) -> State<'a> {
    let mut initial_state =
        raw.initial_state(cfg.max_sat_per_gw.0, cfg.max_gw_per_sat.0, &cfg.skip);
    initial_state.stats();
    initial_state.schedule();

    if let Some(mut path) = dump {
        let (_, stats) = initial_state.nest_handovers(cfg.max_delay);
        path.push("stats");
        path.set_extension("json");
        if let Ok(contents) = serde_json::to_string(&stats) {
            let _ = write(path, contents);
        } else {
            warn!("failed to serialize stats");
        }
    }
    initial_state
}

#[derive(Debug, PartialEq)]
pub struct State<'a> {
    // Map for each gw the sats for which it is connected.
    all_sat_connected: SatConnectedPerGw<'a>,
    // Map for each gw its sat candidates.
    all_sat_candidates: SatCandidatesPerGw<'a>,
    // Map for each sat the gws for which it is connected.
    all_gw_connected: GwConnectedPerSat<'a>,
    // Map for each sat its  gw candidates (+ data for candidate choice).
    all_gw_candidates: GwCandidatesPerSat<'a>,
    connectivity_events: ConnectivityEvents<'a>,
    max_sat_per_gw: usize,
    max_gw_per_sat: usize,
    // Global history by timestamp.
    history: HashMap<Timestamp, Vec<(&'a GatewayName, &'a SatPos, HandoverType, Option<f64>)>>,
    // Orbital period of the constellation.
    period: f64,
}

#[derive(Serialize, Debug, PartialEq, Clone)]
pub struct NestingStats {
    total: u64,
    nested: u64,
}

#[derive(Serialize, Debug, PartialEq, Clone)]
pub struct Stats {
    nesting: NestingStats,
    delays: Vec<u64>,
    period: f64,
    durations: Vec<Timestamp>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EmitHandover<'a> {
    Handover {
        gw: &'a GatewayName,
        sat_addition: &'a SatPos,
        sat_removal: &'a SatPos,
        delay: Timestamp,
    },
    ConnectivityEvent {
        gw: &'a GatewayName,
        sat: &'a SatPos,
        event_type: &'a HandoverType,
    },
}

impl<'a> EmitHandover<'a> {
    pub fn get_gateway(&self) -> &'a String {
        match self {
            Self::Handover { gw, .. } => gw,
            Self::ConnectivityEvent { gw, .. } => gw,
        }
    }
}

pub fn nesting_stats<'a>(
    handovers: &Vec<(u64, EmitHandover<'a>)>,
    period: f64,
    durations: Vec<Timestamp>,
) -> Stats {
    let (total, nested, delays) = handovers.iter().fold(
        (0, 0, vec![]),
        |(total, nested, mut delays): (u64, u64, Vec<u64>), (_, handover)| match handover {
            EmitHandover::Handover { delay, .. } => {
                delays.push(*delay);
                (total + 2, nested + 2, delays)
            }
            EmitHandover::ConnectivityEvent { .. } => (total + 1, nested, delays),
        },
    );
    info!(
        ?total,
        ?nested,
        nesting_ratio = format!("{:.3}", ((nested as f64 / total as f64) * 100f64))
    );
    Stats {
        nesting: NestingStats { total, nested },
        delays,
        period,
        durations,
    }
}

pub fn nest_handovers<'a>(
    gw: &'a GatewayName,
    // Events are sorted by increasing ts.
    connectivity_events: &'a Vec<(Timestamp, HandoverType, &'a SatPos)>,
    max_delay: u64,
) -> Vec<(Timestamp, EmitHandover<'a>)> {
    let mut previous_nested = false;
    let mut ret: Vec<(Timestamp, EmitHandover<'a>)> = connectivity_events
        .iter()
        .zip(connectivity_events[1..].iter())
        .filter_map(|(first_ce, second_ce)| {
            debug!(?first_ce, ?second_ce);
            let (ts1, ht1, sat1) = first_ce;
            let (ts2, ht2, sat2) = second_ce;

            if previous_nested {
                previous_nested = false;
                debug!("skipped");
                None
            } else if (ts1 == ts2 && ht1 != ht2)
                || (ts1 <= ts2
                    && matches!(ht1, HandoverType::Addition)
                    && matches!(ht2, HandoverType::Removal)
                    && ts2 - ts1 <= max_delay)
            {
                previous_nested = true;
                debug!("nested");
                Some((
                    *ts2,
                    EmitHandover::Handover {
                        gw,
                        sat_removal: sat2,
                        sat_addition: sat1,
                        delay: ts2 - ts1,
                    },
                ))
            } else {
                debug!("first emitted");
                Some((
                    *ts1,
                    EmitHandover::ConnectivityEvent {
                        gw,
                        sat: sat1,
                        event_type: ht1,
                    },
                ))
            }
        })
        .collect();

    // If last pair has not been nested, we must emit the last handover as a single one.
    // The zip explores the sequence as:
    // 2 3 X
    // 1 2 3
    // The last iteration of the zip is (2, 3). If that pair is nested then 3 is included
    // in the handover else (3, X) is not explored by the zip, hence we add the last handover.
    if !previous_nested {
        let (ts, ht, sat) = connectivity_events.last().unwrap();
        ret.push((
            *ts,
            EmitHandover::ConnectivityEvent {
                gw,
                sat,
                event_type: ht,
            },
        ));
    }

    ret
}

impl<'a> State<'a> {
    pub fn stats(&self) {
        let sats_per_gw = self
            .all_sat_connected
            .iter()
            .map(|(gw, sats)| (*gw, sats.len()))
            .collect::<HashMap<&GatewayName, usize>>();

        let gw_per_sats: HashMap<&SatPos, usize> = self
            .all_gw_connected
            .iter()
            .map(|(sat, gws)| (*sat, gws.len()))
            .collect();

        info!(?sats_per_gw, ?gw_per_sats);
    }

    pub fn connection_durations(&self) -> HashMap<&'a GatewayName, Vec<Timestamp>> {
        self.connectivity_events
            .iter()
            .map(|(&gw, handovers)| {
                let mut pending: HashMap<&'a SatPos, Timestamp> = Default::default();
                let durations: Vec<Timestamp> = handovers
                    .iter()
                    .filter_map(|(ts, ht, sat)| match ht {
                        HandoverType::Addition => {
                            *pending.entry(sat).or_default() = *ts;
                            None
                        }
                        HandoverType::Removal => {
                            Some(ts - pending.remove(sat).or(Some(0)).unwrap())
                        }
                    })
                    .collect();
                (gw, durations)
            })
            .collect()
    }

    pub fn nest_handovers(
        &'a self,
        max_delay: u64,
    ) -> (FilteredEvents<'a>, HashMap<&'a GatewayName, Stats>) {
        self.connectivity_events
            .iter()
            .map(|(gw, handovers)| {
                let _span = info_span!("", ?gw).entered();
                let mut pending: HashMap<&'a SatPos, Timestamp> = Default::default();
                let durations: Vec<Timestamp> = handovers
                    .iter()
                    .filter_map(|(ts, ht, sat)| match ht {
                        HandoverType::Addition => {
                            *pending.entry(sat).or_default() = *ts;
                            None
                        }
                        HandoverType::Removal => {
                            Some(ts - pending.remove(sat).or(Some(0)).unwrap())
                        }
                    })
                    .collect();
                let handovers = nest_handovers(gw, handovers, max_delay);
                let stats = nesting_stats(&handovers, self.period, durations);
                ((*gw, handovers), (*gw, stats))
            })
            .collect()
    }

    fn try_connect_sat_to_gw(
        &mut self,
        sat: &'a SatPos,
        gw_candidate: &'a GatewayName,
        ts: Timestamp,
    ) -> bool {
        let _span = debug_span!("try_connect_sat_to_gw").entered();

        let gw_connected = self.all_gw_connected.entry(sat).or_default();
        let gw_candidates = self.all_gw_candidates.entry(sat).or_default();
        let gw_slots = self.all_sat_connected.entry(gw_candidate).or_default();
        if gw_slots.len() < self.max_sat_per_gw {
            // We have a match! Connect the gw to the sat.
            gw_slots.insert(sat);
            gw_connected.insert(gw_candidate);

            // Cleanup candidate lists.
            gw_candidates.remove(gw_candidate);
            self.all_sat_candidates
                .entry(gw_candidate)
                .and_modify(|entry| {
                    entry.remove(sat);
                });
            info!(gw=?gw_candidate, "new connection");
            self.connectivity_events
                .entry(gw_candidate)
                .or_default()
                .push((ts, HandoverType::Addition, sat));
            true
        } else {
            debug!(?gw_candidate, "no slot available");
            false
        }
    }

    pub fn try_connect_sat(&mut self, sat: &'a SatPos, ts: Timestamp) {
        let _span = debug_span!("try_connect_sat").entered();
        // TODO: Sort candidates by 1. longest time window, and 2. distance as tie-break.
        let gw_candidates = self.all_gw_candidates.entry(sat).or_default();
        let mut candidates: Vec<&GatewayName> = gw_candidates.keys().cloned().collect();
        candidates.sort();
        for candidate in candidates {
            if self.try_connect_sat_to_gw(sat, candidate, ts) {
                break;
            }
        }
    }

    fn try_connect_gw_to_sat(
        &mut self,
        gw: &'a GatewayName,
        sat_candidate: &'a SatPos,
        ts: Timestamp,
    ) -> bool {
        let sat_candidates = self.all_sat_candidates.entry(gw).or_default();
        let sat_connected = self.all_sat_connected.entry(gw).or_default();

        // Check whether the sat candidate has an available slot.
        let sat_slots = self.all_gw_connected.entry(sat_candidate).or_default();
        if sat_slots.len() < self.max_gw_per_sat {
            // We have a match! Connect the sat and gw.
            sat_slots.insert(gw);
            sat_connected.insert(sat_candidate);

            // Cleanup candidate lists.
            sat_candidates.remove(sat_candidate);
            self.all_gw_candidates
                .entry(sat_candidate)
                .and_modify(|entry| {
                    entry.remove(gw);
                });

            info!(sat=?sat_candidate, "new connection");
            self.connectivity_events.entry(gw).or_default().push((
                ts,
                HandoverType::Addition,
                sat_candidate,
            ));
            true
        } else {
            warn!(?sat_candidate, "no slot available");
            false
        }
    }

    fn try_connect_gw(&mut self, gw: &'a GatewayName, ts: Timestamp) {
        let _span = info_span!("try_connect_gw").entered();

        // TODO: Sort candidates by 1. longest time window, and 2. distance as tie-break.
        let sat_candidates = self.all_sat_candidates.entry(gw).or_default();
        let mut candidates: Vec<&SatPos> = sat_candidates.keys().cloned().collect();
        candidates.sort();
        for candidate in candidates {
            if self.try_connect_gw_to_sat(gw, candidate, ts) {
                break;
            }
        }
    }

    fn handle_event(
        &mut self,
        gw: &'a GatewayName,
        sat: &'a SatPos,
        ht: HandoverType,
        distance: Distance,
        ts: Timestamp,
    ) {
        let _span2 = info_span!("", ?gw, ?sat, ?ht).entered();
        if let Some(distance) = distance {
            debug!(?distance);
        } else {
            warn!("no distance");
        }

        // Get data from the gw point of view.
        let sat_connected = self.all_sat_connected.entry(gw).or_default();
        let sat_candidates = self.all_sat_candidates.entry(gw).or_default();

        // Get data from the sat point of view.
        let gw_connected = self.all_gw_connected.entry(sat).or_default();
        let gw_candidates = self.all_gw_candidates.entry(sat).or_default();

        debug!(?sat_connected, sat_candidates=?sat_candidates.keys(), "gw state");
        debug!(?gw_connected, gw_candidates=?gw_candidates.keys(), "sat state");

        match ht {
            // Sat leaves the sight of gw.
            HandoverType::Removal => {
                if sat_connected.contains(sat) && gw_connected.contains(gw) {
                    // If the connection is correctly established, we remove it.
                    sat_connected.remove(sat);
                    gw_connected.remove(gw);
                    info!("valid removal");
                    self.connectivity_events
                        .entry(gw)
                        .or_default()
                        .push((ts, ht.clone(), sat));

                    // We removed a valid connection, both the sat and gw should try a new
                    // connection if they have candidates.
                    self.try_connect_sat(sat, ts);
                    self.try_connect_gw(gw, ts);
                } else if sat_candidates.contains_key(sat) && gw_candidates.contains_key(gw) {
                    // If the connection is in the candidate sat, it is removed.

                    sat_candidates.remove(sat);
                    gw_candidates.remove(gw);
                    debug!(sat_candidates=?sat_candidates.keys(), gw_candidates=?gw_candidates.keys(), "candidate sat leaving sight");
                } else {
                    error!("asymmetric connection between sat and gw");
                }
            }
            // Sat enters the sights of gw.
            HandoverType::Addition => {
                if sat_connected.len() < self.max_sat_per_gw
                    && gw_connected.len() < self.max_gw_per_sat
                {
                    // If both the gw and the sat have some bw available, they connect.
                    sat_connected.insert(sat);
                    gw_connected.insert(gw);
                    info!("valid addition");
                    debug!(?sat_connected, ?gw_connected);
                    self.connectivity_events
                        .entry(gw)
                        .or_default()
                        .push((ts, ht.clone(), sat));

                    // Sanity check, if they were forgotten candidates.
                    sat_candidates.remove(sat);
                    gw_candidates.remove(gw);
                } else {
                    // At least one end of the connection is not available, add the connection
                    // to the candidate set.
                    let co_data = ConnectionData {
                        distance: distance,
                        ts_lost_of_sight: 0,
                    };
                    sat_candidates.insert(sat, co_data.clone());
                    gw_candidates.insert(gw, co_data);
                    debug!(
                        sat_candidates=?sat_candidates.keys(),
                        gw_candidates=?gw_candidates.keys(),
                        "gw or sat is full, add connection to candidates",
                    );
                }

                for (gw, sat_connected) in self.all_sat_connected.iter() {
                    assert!(
                        sat_connected.len() <= self.max_sat_per_gw,
                        "{gw:?}: {sat_connected:?}"
                    );
                }

                for (sat, gw_connected) in self.all_gw_connected.iter() {
                    assert!(
                        gw_connected.len() <= self.max_gw_per_sat,
                        "{sat:?}: {gw_connected:?}"
                    );
                }
            }
        }
    }

    pub fn schedule(&mut self) -> &ConnectivityEvents<'a> {
        if self.connectivity_events.is_empty() {
            let mut times: Vec<Timestamp> = self.history.keys().cloned().collect();
            times.sort();

            for ts in times.iter() {
                let _span = info_span!("", ?ts).entered();
                for (gw, sat, ht, distance) in self.history[ts].clone() {
                    self.handle_event(gw, sat, ht, distance, *ts);
                }
            }
        }
        &self.connectivity_events
    }
}
