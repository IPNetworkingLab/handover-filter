# Handover Filter

A simple Rust crate that defines a physically possible (maybe not optimal) sequence of connectivity events.

It applies some simple rules to first define a possible initial state where each radio of each ground station is attributed with the longest feeder link possible, if any.
Then, for each connectivity event, it estimates whether the event is applicable (e.g., does the ground station/satellite have enough available radio to establish a feeder link, is the potential feeder long enough to trigger a handover, ...).

This produces, for each ground station, a sequence of connectivity events sorted by timestamp.
Some of them are simple, i.e., a new feeder link is added or an old feeder link is removed; and some are compound, i.e., both events happen simultaneously.

If a dump path is specified, such schedule is dumped in a `json` file.
