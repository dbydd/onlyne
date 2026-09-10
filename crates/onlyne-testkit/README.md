# onlyne-testkit

The test kit provides `HostSim`, `FakeAgent`, and `FakeGateway` fixtures for adapter protocol conformance.

## Fake agent script

`onlyne-agent-fake` reads one JSON object from `--script FILE` or, with `--stdin-script`, standard input. The script shape is:

```json
{"hello":{"capabilities":["register","report","inject","recycle"]},"steps":[{"wait_assign":true},{"report":"ready"},{"complete":{"outcome":"done","head_from":"assign_body"}},{"echo_prose_to":"prose.log"}]}
```

Supported steps are `wait_assign`, `report` (`ready` or `heartbeat`), `complete`, `fail`, `exit`, `sleep_ms`, `assert_prose_equals`, `assert_field`, and `echo_prose_to`. Unknown steps fail with a message naming the step. `--capabilities` takes a comma-separated capability list and overrides the script hello list. `--workspace DIR` resolves `DIR/.onlyne/run/s`; `--socket PATH` overrides it. `--role NAME` sets the agent role. `--once` exits after the script completes.

## Fake gateway

`onlyne-gateway-fake --platform fake --gateway-id fg1 --socket PATH` connects as a gateway mount. Each host `render_send` is printed as `{"op":"rendered","conversation":...,"text":...,"has_image":...}`. Each stdin line `{"op":"inbound","conversation":"c1","text":"hello"}` sends a `deliver` frame with `Principal::Gateway` as its sender.

## Backend choice

The three-way fixture uses the `onlyne-session` fake backend when that crate compiles. The current workspace resolves it as `onlyne-session:fake`.
