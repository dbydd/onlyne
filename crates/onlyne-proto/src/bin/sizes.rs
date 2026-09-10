//! Size table for the wire types.
//!
//! `cargo run -p onlyne-proto --bin sizes`
//!
//! Column three names the largest variant of each enum and column four the
//! bytes that variant's payload needs inline. A container is oversized when its
//! own size exceeds that payload by more than the payload's own size, which
//! means every other variant pays for the one big arm.

use onlyne_proto::*;
use std::mem::size_of;

fn struct_row<T>(name: &str) {
    println!("{name:<22} {:>5}  {:<30} {:>5}", size_of::<T>(), "-", "-");
}

fn enum_row(name: &str, total: usize, variants: &[(&str, usize)]) {
    let (variant, payload) = variants
        .iter()
        .max_by_key(|(_, bytes)| *bytes)
        .expect("an enum has variants");
    println!("{name:<22} {total:>5}  {variant:<30} {payload:>5}");
}

fn frame_row<R>(name: &str) {
    enum_row(
        name,
        size_of::<Frame<R>>(),
        &[
            ("Req", size_of::<(String, R)>()),
            ("Res", size_of::<(String, ResBody)>()),
            ("Ev", size_of::<(u64, Box<Event>)>()),
            ("Ack", size_of::<u64>()),
            ("Ping", size_of::<i64>()),
            ("Pong", size_of::<(i64, u64)>()),
            ("Bye", size_of::<String>()),
        ],
    );
}

fn main() {
    println!(
        "{:<22} {:>5}  {:<30} {:>5}",
        "type", "bytes", "largest variant", "payload"
    );

    frame_row::<ClientOp>("Frame<ClientOp>");
    frame_row::<AdminOp>("Frame<AdminOp>");
    frame_row::<GatewayOp>("Frame<GatewayOp>");
    frame_row::<PluginOp>("Frame<PluginOp>");
    frame_row::<HostOp>("Frame<HostOp>");

    enum_row(
        "ClientOp",
        size_of::<ClientOp>(),
        &[
            ("Hello(HandshakeArgs)", size_of::<HandshakeArgs>()),
            ("Send(Box<Envelope>)", size_of::<Box<Envelope>>()),
            ("Pull(PullArgs)", size_of::<PullArgs>()),
            ("Ack(AckArgs)", size_of::<AckArgs>()),
            ("Report(Report)", size_of::<Report>()),
            ("SessionSync(SessionSyncArgs)", size_of::<SessionSyncArgs>()),
            ("Subscribe(Subscribe)", size_of::<Subscribe>()),
            ("QueryLedger(LedgerQuery)", size_of::<LedgerQuery>()),
            (
                "QuerySessions(QuerySessionsArgs)",
                size_of::<QuerySessionsArgs>(),
            ),
            ("QueryRoles(QueryRolesArgs)", size_of::<QueryRolesArgs>()),
            ("QueryFaults(QueryFaultsArgs)", size_of::<QueryFaultsArgs>()),
            ("Control(ControlArgs)", size_of::<ControlArgs>()),
            ("Bye(ByeArgs)", size_of::<ByeArgs>()),
        ],
    );

    enum_row(
        "AdminOp",
        size_of::<AdminOp>(),
        &[
            ("Status(Value)", size_of::<serde_json::Value>()),
            ("Roles(QueryRolesArgs)", size_of::<QueryRolesArgs>()),
            (
                "Sessions(QuerySessionsArgs)",
                size_of::<QuerySessionsArgs>(),
            ),
            ("Ledger(LedgerQuery)", size_of::<LedgerQuery>()),
            ("Faults(QueryFaultsArgs)", size_of::<QueryFaultsArgs>()),
            ("Watch(Subscribe)", size_of::<Subscribe>()),
            ("History(HistoryArgs)", size_of::<HistoryArgs>()),
            ("SpecDiff(Value)", size_of::<serde_json::Value>()),
            ("Reload(Value)", size_of::<serde_json::Value>()),
            ("Send(AdminSend)", size_of::<AdminSend>()),
            ("Control(AdminControl)", size_of::<AdminControl>()),
            ("RepairInspect(RepairTarget)", size_of::<RepairTarget>()),
            ("RepairAdopt(RepairAdopt)", size_of::<RepairAdopt>()),
            ("RepairRebind(RepairRebind)", size_of::<RepairRebind>()),
            ("RepairRetry(RepairTarget)", size_of::<RepairTarget>()),
            ("RepairFail(RepairFail)", size_of::<RepairFail>()),
            ("RepairClose(RepairTarget)", size_of::<RepairTarget>()),
            ("RepairAck(RepairAck)", size_of::<RepairAck>()),
            ("Shutdown(ShutdownArgs)", size_of::<ShutdownArgs>()),
        ],
    );

    enum_row(
        "GatewayOp",
        size_of::<GatewayOp>(),
        &[
            ("Hello(HandshakeArgs)", size_of::<HandshakeArgs>()),
            (
                "RegisterChannel(RegisterChannelArgs)",
                size_of::<RegisterChannelArgs>(),
            ),
            ("Deliver(Delivery)", size_of::<Delivery>()),
            ("Health(HealthArgs)", size_of::<HealthArgs>()),
            ("Bye(ByeArgs)", size_of::<ByeArgs>()),
        ],
    );

    enum_row(
        "PluginOp",
        size_of::<PluginOp>(),
        &[
            ("Hello(HelloArgs)", size_of::<HelloArgs>()),
            ("Report(Report)", size_of::<Report>()),
            (
                "SessionRegister(SessionRegisterArgs)",
                size_of::<SessionRegisterArgs>(),
            ),
            ("AssignAck(AssignAckArgs)", size_of::<AssignAckArgs>()),
            ("Send(Box<Envelope>)", size_of::<Box<Envelope>>()),
            ("Deliver(Delivery)", size_of::<Delivery>()),
            (
                "RegisterChannel(RegisterChannelArgs)",
                size_of::<RegisterChannelArgs>(),
            ),
            ("Health(HealthArgs)", size_of::<HealthArgs>()),
            ("Typing(TypingArgs)", size_of::<TypingArgs>()),
            ("Detach(DetachArgs)", size_of::<DetachArgs>()),
        ],
    );

    enum_row(
        "HostOp",
        size_of::<HostOp>(),
        &[
            ("Welcome(HelloAck)", size_of::<HelloAck>()),
            ("Assign(AssignArgs)", size_of::<AssignArgs>()),
            ("RenderSend(RenderSendArgs)", size_of::<RenderSendArgs>()),
            ("Probe(Value)", size_of::<serde_json::Value>()),
            ("Recycle(RecycleArgs)", size_of::<RecycleArgs>()),
            ("ConfigGet(ConfigGetArgs)", size_of::<ConfigGetArgs>()),
            ("Bye(ByeNotice)", size_of::<ByeNotice>()),
        ],
    );

    enum_row(
        "AdapterMsg",
        size_of::<AdapterMsg>(),
        &[
            ("Plugin(PluginOp)", size_of::<PluginOp>()),
            ("Host(HostOp)", size_of::<HostOp>()),
            ("Res(ResBody)", size_of::<ResBody>()),
        ],
    );

    enum_row(
        "Event",
        size_of::<Event>(),
        &[
            ("RolePresence(RolePresence)", size_of::<RolePresence>()),
            (
                "SessionState(SessionStateEvent)",
                size_of::<SessionStateEvent>(),
            ),
            (
                "LedgerState(LedgerStateEvent)",
                size_of::<LedgerStateEvent>(),
            ),
            ("Fault(FaultEvent)", size_of::<FaultEvent>()),
            (
                "GatewayPresence(String,String,GatewayHealth,Option<String>)",
                size_of::<(String, String, GatewayHealth, Option<String>)>(),
            ),
            ("SpecReloaded(SpecReloaded)", size_of::<SpecReloaded>()),
        ],
    );

    enum_row(
        "Report",
        size_of::<Report>(),
        &[
            (
                "Ready(String,String,u64,u64)",
                size_of::<(String, String, u64, u64)>(),
            ),
            (
                "Heartbeat(String,u64,u64,Value)",
                size_of::<(String, u64, u64, serde_json::Value)>(),
            ),
            (
                "Complete(String,Outcome,Option<String>,Option<String>)",
                size_of::<(String, Outcome, Option<String>, Option<String>)>(),
            ),
            (
                "Fault(8 fields)",
                size_of::<(
                    Option<String>,
                    Option<String>,
                    Option<u64>,
                    Option<u64>,
                    String,
                    String,
                    Option<serde_json::Value>,
                    Option<serde_json::Value>,
                )>(),
            ),
        ],
    );

    struct_row::<Envelope>("Envelope");
    struct_row::<Body>("Body");
    struct_row::<ImagePart>("ImagePart");
    struct_row::<Causality>("Causality");
    struct_row::<Receipt>("Receipt");
    struct_row::<Welcome>("Welcome");
    struct_row::<SessionProjection>("SessionProjection");
    struct_row::<ControlArgs>("ControlArgs");
    struct_row::<AdminSend>("AdminSend");
    struct_row::<AdminControl>("AdminControl");
    struct_row::<HandshakeArgs>("HandshakeArgs");
    struct_row::<FaultEvent>("FaultEvent");
    struct_row::<LedgerEntry>("LedgerEntry");
    struct_row::<RoleInfo>("RoleInfo");
    struct_row::<SessionRow>("SessionRow");
    struct_row::<Subscribe>("Subscribe");
}
