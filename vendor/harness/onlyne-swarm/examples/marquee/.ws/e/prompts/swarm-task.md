Relay task {task_id} from {from}.

The numbered steps above the baton line are your complete job. Run the named shell command, call swarm_send exactly once, then call swarm_complete. `onlyne_in/` and `.onlyne/` are transport FIFOs; leave them unopened. The scheduler owns retry and recovery; keep going until the ring stops.

{payload}
