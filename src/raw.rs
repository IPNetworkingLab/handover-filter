use std::{collections::HashMap, str::FromStr};

use ordered_float::OrderedFloat;
use serde::{Deserialize, Serialize};
use tracing::{info, info_span, warn};

use crate::{
    ConnectionData, GatewayEvents, GatewayName, GwCandidatesPerSat, GwConnectedPerSat, Handover,
    HandoverType, SatCandidatesPerGw, SatConnectedPerGw, SatHistory, SatPos, State, Timestamp,
};

/// File format from the kepler computer.
#[derive(Deserialize, Serialize, Debug)]
pub struct RawHandovers {
    /// For each gw, the sats in sight at ts=0.
    pub initial: HashMap<String, Vec<SatPos>>,
    /// For eacg gw, the history of sats entering/leaving the sight cone.
    pub history: HashMap<GatewayName, GatewayEvents>,
    /// Orbital period of the constellation.
    pub period: f64,
}

impl RawHandovers {
    /// Compute history for each sat based on initial state and history from the gws
    /// viewpoint.
    fn collect_sats_history<'a>(
        &'a self,
        skip: &Vec<GatewayName>,
    ) -> HashMap<&'a SatPos, SatHistory<'a>> {
        let mut all_add_events = 0;
        let mut all_del_events = 0;
        let mut instantaneous = 0;
        let mut sat_history: HashMap<&'a SatPos, SatHistory<'a>> = Default::default();

        let mut gws = self.initial.keys().collect::<Vec<&String>>();
        gws.sort();

        gws.iter().for_each(|&gw| {
            let _span = info_span!("", ?gw).entered();
            if let Some(events) = self.history.get(gw)
                && !skip.contains(gw)
            {
                // Collect timestamp to iterate the history.
                let mut times: Vec<&Timestamp> = events.keys().collect();
                times.sort();

                // For each sat, collect its history of entering/leaving gws sight cone.
                // Also count number of entering/leaving events for the gw.
                let (add_events, del_events) = times.iter().fold((0, 0), |(add, del), ts| {
                    let add_events = &events[ts].get("add");
                    let del_events = &events[ts].get("del");

                    // Look for instantaneous handovers.
                    if let Some(add_events) = add_events
                        && let Some(del_events) = del_events
                    {
                        // Got an instantaneous handover.
                        info!(?ts, ?add_events, ?del_events, "instantaneous");
                        instantaneous += 1;
                    }

                    if let Some(events) = add_events {
                        events.iter().for_each(|handover| {
                            sat_history
                                .entry(&handover.sat)
                                .or_default()
                                .entry(**ts)
                                .or_default()
                                .push((gw, HandoverType::Addition));
                        });
                    }

                    if let Some(events) = del_events {
                        events.iter().for_each(|handover| {
                            sat_history
                                .entry(&handover.sat)
                                .or_default()
                                .entry(**ts)
                                .or_default()
                                .push((gw, HandoverType::Removal));
                        });
                    }

                    (
                        add + add_events
                            .and_then(|events| Some(events.len()))
                            .unwrap_or(0),
                        del + del_events
                            .and_then(|events| Some(events.len()))
                            .unwrap_or(0),
                    )
                });
                info!(?add_events, ?del_events, total = add_events + del_events);
                all_add_events += add_events;
                all_del_events += del_events;
            } else {
                warn!("No events for {gw}");
            }
        });

        info!(
            ?all_add_events,
            ?all_del_events,
            total = all_add_events + all_del_events,
            ?instantaneous
        );
        sat_history
    }

    fn candidates_by_duration<'a>(
        &'a self,
        sat_history: &HashMap<&'a SatPos, SatHistory<'a>>,
        skip: &Vec<GatewayName>,
    ) -> HashMap<Timestamp, Vec<(&'a GatewayName, &'a SatPos)>> {
        // Build a list of pairs (gw, sat) per connection duration.
        let mut candidates_by_duration: HashMap<Timestamp, Vec<(&GatewayName, &SatPos)>> =
            Default::default();

        for (gw, sats) in &self.initial {
            if skip.contains(gw) {
                continue;
            }
            // Map for each sat the ts at which it is not more in sight of the gateway.
            let time_in_sight: HashMap<&SatPos, Timestamp> = sats
                .iter()
                .filter_map(|sat| {
                    let history = &sat_history[sat];
                    let mut times: Vec<&Timestamp> = history.keys().collect();
                    times.sort();
                    times
                        .iter()
                        .find(|ts| {
                            history[ts]
                                .iter()
                                .find(|(gw_in_sight, evt)| {
                                    gw_in_sight == &gw && matches!(evt, HandoverType::Removal)
                                })
                                .is_some()
                        })
                        .map(|ts| (sat, **ts))
                })
                .collect();

            time_in_sight.iter().for_each(|(sat, ts)| {
                // TODO: Sort candidates at the same duration by increasing distance with their gw.
                candidates_by_duration
                    .entry(*ts)
                    .or_default()
                    .push((gw, sat));
            });
        }
        candidates_by_duration
    }

    fn initial_allocate<'a>(
        &'a self,
        candidates_by_duration: &HashMap<Timestamp, Vec<(&'a GatewayName, &'a SatPos)>>,
        max_sat_per_gw: usize,
        max_gw_per_sat: usize,
    ) -> (
        SatConnectedPerGw<'a>,
        GwConnectedPerSat<'a>,
        SatCandidatesPerGw<'a>,
        GwCandidatesPerSat<'a>,
    ) {
        let mut initial_connected_per_gw: SatConnectedPerGw<'a> = Default::default();
        let mut initial_connected_per_sat: GwConnectedPerSat<'a> = Default::default();
        let mut initial_candidate_per_gw: SatCandidatesPerGw<'a> = Default::default();
        let mut initial_candidate_per_sat: GwCandidatesPerSat<'a> = Default::default();

        let mut ordering: Vec<&Timestamp> = candidates_by_duration.keys().collect();
        ordering.sort();
        ordering.reverse();
        ordering.iter().for_each(|ts| {
            // info!(?ts, candidates=?candidates_by_duration[ts]);
            candidates_by_duration[ts]
                .iter()
                .for_each(|(candidate_gw, candidate_sat)| {
                    if let gw_connections =
                        initial_connected_per_gw.entry(candidate_gw).or_default()
                        && gw_connections.len() < max_sat_per_gw
                        && let sat_connnections =
                            initial_connected_per_sat.entry(candidate_sat).or_default()
                        && sat_connnections.len() < max_gw_per_sat
                        && !gw_connections.contains(candidate_sat)
                        && !sat_connnections.contains(candidate_gw)
                    {
                        // The candidate connection is valid for both constraints, we accept it.
                        gw_connections.insert(candidate_sat);
                        sat_connnections.insert(candidate_gw);
                    } else {
                        let co_data = ConnectionData {
                            distance: None,
                            ts_lost_of_sight: **ts,
                        };
                        initial_candidate_per_gw
                            .entry(*candidate_gw)
                            .or_default()
                            .entry(candidate_sat)
                            .or_insert(co_data.clone());

                        initial_candidate_per_sat
                            .entry(*candidate_sat)
                            .or_default()
                            .entry(candidate_gw)
                            .or_insert(co_data);
                    }
                });
        });
        (
            initial_connected_per_gw,
            initial_connected_per_sat,
            initial_candidate_per_gw,
            initial_candidate_per_sat,
        )
    }

    fn build_history<'a>(
        &'a self,
        skip: &Vec<GatewayName>,
    ) -> HashMap<Timestamp, Vec<(&'a GatewayName, &'a SatPos, HandoverType, Option<f64>)>> {
        // Build a flattened history.
        let mut sorting: HashMap<
            Timestamp,
            HashMap<HandoverType, HashMap<Option<OrderedFloat<f64>>, Vec<(&SatPos, &GatewayName)>>>,
        > = Default::default();

        self.history.iter().for_each(|(gw, events)| {
            events.iter().for_each(|(ts, events)| {
                events.iter().for_each(|(ht, handovers)| {
                    handovers.iter().for_each(|Handover { sat, distance }| {
                        if !skip.contains(gw) {
                            sorting
                                .entry(*ts)
                                .or_default()
                                .entry(HandoverType::from_str(ht).unwrap())
                                .or_default()
                                .entry(distance.and_then(|dist| Some(OrderedFloat(dist))))
                                .or_default()
                                .push((sat, gw));
                        }
                    });
                });
            });
        });

        fn order_events<'a>(
            sorting: &HashMap<
                HandoverType,
                HashMap<Option<OrderedFloat<f64>>, Vec<(&'a SatPos, &'a String)>>,
            >,
            kind: &HandoverType,
            ts: &Timestamp,
            history: &mut HashMap<
                Timestamp,
                Vec<(&'a GatewayName, &'a SatPos, HandoverType, Option<f64>)>,
            >,
        ) {
            if let Some(events) = sorting.get(kind) {
                let mut distance_ordering: Vec<&Option<OrderedFloat<f64>>> =
                    events.keys().collect();
                // If multiple addition, sort by increasing distance.
                distance_ordering.sort();
                if matches!(kind, &HandoverType::Removal) {
                    // If multiple removal, sort by deacreasing distance.
                    distance_ordering.reverse();
                }
                distance_ordering.iter().for_each(|dist| {
                    events[dist].iter().for_each(|(sat, gw)| {
                        history.entry(*ts).or_default().push((
                            gw,
                            sat,
                            kind.clone(),
                            dist.and_then(|d| Some(*d)),
                        ));
                    });
                });
            }
        }

        let mut history: HashMap<
            Timestamp,
            Vec<(&GatewayName, &SatPos, HandoverType, Option<f64>)>,
        > = Default::default();

        // Tie-breaks.
        sorting.iter().for_each(|(ts, sorting)| {
            // First apply connection removals
            order_events(sorting, &HandoverType::Removal, ts, &mut history);
            // Then connection additions
            order_events(sorting, &HandoverType::Addition, ts, &mut history);
        });
        history
    }

    pub fn initial_state<'a>(
        &'a self,
        max_sat_per_gw: usize,
        max_gw_per_sat: usize,
        skip: &Vec<GatewayName>,
    ) -> State<'a> {
        let sat_history = self.collect_sats_history(&skip);
        let candidates_by_duration = self.candidates_by_duration(&sat_history, &skip);
        let (
            initial_connected_per_gw,
            initial_connected_per_sat,
            initial_candidate_per_gw,
            initial_candidate_per_sat,
        ) = self.initial_allocate(&candidates_by_duration, max_sat_per_gw, max_gw_per_sat);
        let history = self.build_history(skip);

        State {
            all_sat_connected: initial_connected_per_gw,
            all_sat_candidates: initial_candidate_per_gw,
            all_gw_connected: initial_connected_per_sat,
            all_gw_candidates: initial_candidate_per_sat,
            connectivity_events: Default::default(),
            max_sat_per_gw,
            max_gw_per_sat,
            history,
            period: self.period,
        }
    }
}

#[cfg(test)]
mod test {

    use super::*;
    use std::collections::HashSet;

    type Constellation = (
        GatewayName,
        GatewayName,
        SatPos,
        SatPos,
        SatPos,
        SatPos,
        SatPos,
    );

    struct ConstellationRef<'a> {
        gw1: &'a GatewayName,
        gw2: &'a GatewayName,
        sat0: &'a SatPos,
        sat1: &'a SatPos,
        sat2: &'a SatPos,
        sat3: &'a SatPos,
        sat4: &'a SatPos,
    }

    impl<'a> From<&'a Constellation> for ConstellationRef<'a> {
        fn from((gw1, gw2, sat0, sat1, sat2, sat3, sat4): &'a Constellation) -> Self {
            ConstellationRef {
                gw1: &gw1,
                gw2: &gw2,
                sat0: &sat0,
                sat1: &sat1,
                sat2: &sat2,
                sat3: &sat3,
                sat4: &sat4,
            }
        }
    }

    fn constellation() -> Constellation {
        let gw1 = "gw1".to_string();
        let gw2 = "gw2".to_string();
        let sat0 = SatPos { plane: 0, rank: 0 };
        let sat1 = SatPos { plane: 0, rank: 1 };
        // Initially not in sight of any gw.
        let sat2 = SatPos { plane: 0, rank: 2 };
        let sat3 = SatPos { plane: 0, rank: 3 };
        // Initially in sight of both gws.
        let sat4 = SatPos { plane: 0, rank: 4 };
        (gw1, gw2, sat0, sat1, sat2, sat3, sat4)
    }

    fn raw() -> RawHandovers {
        let (gw1, gw2, sat0, sat1, sat2, sat3, sat4, ..) = constellation();
        let raw = RawHandovers {
            initial: HashMap::from_iter(vec![
                (gw1.clone(), vec![sat0.clone(), sat1.clone(), sat4.clone()]),
                (gw2.clone(), vec![sat3.clone()]),
            ]),
            history: HashMap::from_iter(vec![
                (
                    gw1.clone(),
                    HashMap::from_iter(vec![
                        (
                            1,
                            HashMap::from_iter(vec![(
                                "del".to_string(),
                                vec![Handover {
                                    sat: sat0.clone(),
                                    distance: None,
                                }],
                            )]),
                        ),
                        (
                            3,
                            HashMap::from_iter(vec![(
                                "del".to_string(),
                                vec![Handover {
                                    sat: sat4.clone(),
                                    distance: None,
                                }],
                            )]),
                        ),
                        (
                            6,
                            HashMap::from_iter(vec![(
                                "add".to_string(),
                                vec![Handover {
                                    sat: sat2.clone(),
                                    distance: None,
                                }],
                            )]),
                        ),
                        (
                            12,
                            HashMap::from_iter(vec![(
                                "del".to_string(),
                                vec![Handover {
                                    sat: sat1.clone(),
                                    distance: None,
                                }],
                            )]),
                        ),
                        (
                            20,
                            HashMap::from_iter(vec![(
                                "add".to_string(),
                                vec![Handover {
                                    sat: sat3.clone(),
                                    distance: None,
                                }],
                            )]),
                        ),
                    ]),
                ),
                (
                    gw2.clone(),
                    HashMap::from_iter(vec![
                        (
                            8,
                            HashMap::from_iter(vec![(
                                "del".to_string(),
                                vec![Handover {
                                    sat: sat3.clone(),
                                    distance: None,
                                }],
                            )]),
                        ),
                        (
                            14,
                            HashMap::from_iter(vec![(
                                "add".to_string(),
                                vec![Handover {
                                    sat: sat0.clone(),
                                    distance: None,
                                }],
                            )]),
                        ),
                    ]),
                ),
            ]),
            period: 0f64,
        };
        raw
    }

    fn expected_sat_history<'a>(
        c: &ConstellationRef<'a>,
    ) -> HashMap<&'a SatPos, HashMap<u64, Vec<(&'a String, HandoverType)>>> {
        HashMap::from_iter(vec![
            (
                c.sat0,
                HashMap::from_iter(vec![
                    (1, vec![(c.gw1, HandoverType::Removal)]),
                    (14, vec![(c.gw2, HandoverType::Addition)]),
                ]),
            ),
            (
                c.sat1,
                HashMap::from_iter(vec![(12, vec![(c.gw1, HandoverType::Removal)])]),
            ),
            (
                c.sat2,
                HashMap::from_iter(vec![(6, vec![(c.gw1, HandoverType::Addition)])]),
            ),
            (
                c.sat3,
                HashMap::from_iter(vec![
                    (8, vec![(c.gw2, HandoverType::Removal)]),
                    (20, vec![(c.gw1, HandoverType::Addition)]),
                ]),
            ),
            (
                c.sat4,
                HashMap::from_iter(vec![(3, vec![(c.gw1, HandoverType::Removal)])]),
            ),
        ])
    }

    fn expected_candidates_by_duration<'a>(
        c: &ConstellationRef<'a>,
    ) -> HashMap<u64, Vec<(&'a GatewayName, &'a SatPos)>> {
        HashMap::from_iter(vec![
            (12, vec![(c.gw1, c.sat1)]),
            (8, vec![(c.gw2, c.sat3)]),
            (3, vec![(c.gw1, c.sat4)]),
            (1, vec![(c.gw1, c.sat0)]),
        ])
    }

    fn expected_connected_candidates<'a>(
        cr: ConstellationRef<'a>,
    ) -> (
        SatConnectedPerGw<'a>,
        GwConnectedPerSat<'a>,
        SatCandidatesPerGw<'a>,
        GwCandidatesPerSat<'a>,
    ) {
        let expected_initial_connected_per_gw: SatConnectedPerGw = HashMap::from_iter(vec![
            // Sat0 is filtered out since only 2 slots per gw
            (cr.gw1, HashSet::from_iter(vec![cr.sat1, cr.sat4])),
            (cr.gw2, HashSet::from_iter(vec![cr.sat3])),
        ]);
        let expected_initial_connected_per_sat: GwConnectedPerSat = HashMap::from_iter(vec![
            (cr.sat4, HashSet::from_iter(vec![cr.gw1])),
            (cr.sat1, HashSet::from_iter(vec![cr.gw1])),
            (cr.sat3, HashSet::from_iter(vec![cr.gw2])),
        ]);
        let expected_initial_candidate_per_gw: SatCandidatesPerGw = HashMap::from_iter(vec![(
            cr.gw1,
            HashMap::from_iter(vec![(
                cr.sat0,
                ConnectionData {
                    distance: None,
                    ts_lost_of_sight: 1,
                },
            )]),
        )]);
        let expected_initial_candidate_per_sat: GwCandidatesPerSat = HashMap::from_iter(vec![(
            cr.sat0,
            HashMap::from_iter(vec![(
                cr.gw1,
                ConnectionData {
                    distance: None,
                    ts_lost_of_sight: 1,
                },
            )]),
        )]);
        (
            expected_initial_connected_per_gw,
            expected_initial_connected_per_sat,
            expected_initial_candidate_per_gw,
            expected_initial_candidate_per_sat,
        )
    }

    #[test]
    fn collect_sats_history() {
        let c = constellation();
        let c: ConstellationRef = (&c).into();
        let raw = raw();
        let computed = raw.collect_sats_history(&Default::default());
        let expected = expected_sat_history(&c);
        assert_eq!(expected, computed);
    }

    #[test]
    fn candidates_by_duration() {
        let c = constellation();
        let cr: ConstellationRef = (&c).into();
        let raw = raw();
        let sat_history = expected_sat_history(&cr);
        let computed = raw.candidates_by_duration(&sat_history, &Default::default());
        let expected = expected_candidates_by_duration(&cr);
        assert_eq!(expected, computed);
    }

    #[test]
    fn initial_allocate() {
        let c = constellation();
        let cr: ConstellationRef = (&c).into();
        let raw = raw();

        let max_sat_per_gw = 2;
        let max_gw_per_sat = 1;

        let candidates_by_duration = expected_candidates_by_duration(&cr);
        let (
            computed_initial_connected_per_gw,
            computed_initial_connected_per_sat,
            computed_initial_candidate_per_gw,
            computed_initial_candidate_per_sat,
        ) = raw.initial_allocate(&candidates_by_duration, max_sat_per_gw, max_gw_per_sat);
        let (
            expected_initial_connected_per_gw,
            expected_initial_connected_per_sat,
            expected_initial_candidate_per_gw,
            expected_initial_candidate_per_sat,
        ) = expected_connected_candidates(cr);
        assert_eq!(
            expected_initial_connected_per_sat,
            computed_initial_connected_per_sat
        );
        assert_eq!(
            expected_initial_connected_per_gw,
            computed_initial_connected_per_gw
        );
        assert_eq!(
            expected_initial_candidate_per_gw,
            computed_initial_candidate_per_gw
        );
        assert_eq!(
            expected_initial_candidate_per_sat,
            computed_initial_candidate_per_sat
        );
    }

    #[test]
    fn build_history() {
        let c = constellation();
        let cr: ConstellationRef = (&c).into();
        let raw = raw();
        let computed_history = raw.build_history(&Default::default());
        let expected_history: HashMap<u64, Vec<(&String, &SatPos, HandoverType, Option<f64>)>> =
            HashMap::from_iter(vec![
                (1, vec![(cr.gw1, cr.sat0, HandoverType::Removal, None)]),
                (3, vec![(cr.gw1, cr.sat4, HandoverType::Removal, None)]),
                (6, vec![(cr.gw1, cr.sat2, HandoverType::Addition, None)]),
                (8, vec![(cr.gw2, cr.sat3, HandoverType::Removal, None)]),
                (12, vec![(cr.gw1, cr.sat1, HandoverType::Removal, None)]),
                (14, vec![(cr.gw2, cr.sat0, HandoverType::Addition, None)]),
                (20, vec![(cr.gw1, cr.sat3, HandoverType::Addition, None)]),
            ]);
        assert_eq!(expected_history, computed_history);
    }

    #[test]
    fn initial_state() {
        let max_sat_per_gw = 2;
        let max_gw_per_sat = 1;
        let c = constellation();
        let cr: ConstellationRef = (&c).into();
        let raw = raw();
        let history = raw.build_history(&Default::default());
        let (all_sat_connected, all_gw_connected, all_sat_candidates, all_gw_candidates) =
            expected_connected_candidates(cr);

        let expected = State {
            all_sat_connected,
            all_gw_connected,
            all_sat_candidates,
            all_gw_candidates,
            handovers: Default::default(),
            max_sat_per_gw,
            max_gw_per_sat,
            history,
            period: raw.period,
        };
        let computed = raw.initial_state(max_sat_per_gw, max_gw_per_sat, Default::default());
        assert_eq!(expected, computed);
    }
}
