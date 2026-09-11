# onlyne-testkit

The test kit provides three fixtures for adapter protocol conformance: `HostSim`, `FakeAgent`, and `FakeGateway`.

## Fake agent script

`onlyne-agent-fake` reads one JSON object, either from `--script FILE` or, with `--stdin-script`, from standard input. The script shape is:

```json
{"hello":{"capabilities":["register","report","inject","recycle"]},"steps":[{"wait_assign":true},{"assert_prose_equals":"<prose from the role spec entry>"},{"report":"ready"},{"complete":{"outcome":"done","head_from":"assign_body"}},{"echo_prose_to":"prose.log"}]}
```

Supported steps are `wait_assign`, `report` (`ready` or `heartbeat`), `complete`, `fail`, `exit`, `sleep_ms`, `assert_prose_equals`, `assert_field`, and `echo_prose_to`. An unknown step fails with a message naming the step. `--capabilities` takes a comma-separated capability list and overrides the script hello list. `--workspace DIR` resolves the adapter socket `DIR/.onlyne/run/s` and the mount role from `DIR/.onlyne/config.toml`. `--socket PATH` overrides the socket, and `--role NAME` overrides the role. `--once` exits after the script completes.

## Fake gateway

`onlyne-gateway-fake --platform fake --gateway-id fg1 --socket PATH` connects as a gateway mount. The binary prints each host `render_send` as `{"op":"rendered","conversation":...,"text":...,"has_image":...}`. A stdin line `{"op":"inbound","conversation":"c1","text":"hello"}` sends a `deliver` frame with `Principal::Gateway` as its sender.

The binary prints one `rendered` line per host `render_send`, and `gateway-mount.sh` asserts on that output. Inbound `deliver` frames travel from the fake gateway to the server. The e2e route sends a `Task` to the conversation and asserts that the rendered reply line appears.

The three-way conformance fixture uses the testkit stub because `onlyne-testkit` does not depend on `onlyne-session`.

## Backend choice

The three-way fixture uses the testkit stub backend because only the local crate may declare the `onlyne-session` dependency.
