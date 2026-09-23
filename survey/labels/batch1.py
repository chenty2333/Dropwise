# Labels for batch 1 (omicron, propolis, hyper, h2). Rows not listed default to relevant=no
# by title triage in apply.py. Format: (repo, number, fields).
O, P, H, H2 = "oxidecomputer/omicron", "oxidecomputer/propolis", "hyperium/hyper", "hyperium/h2"

def y(cat, src, cons, inj, layer, found, conf, notes="", fix=""):
    return dict(relevant="yes", category=cat, cancel_source=src, consequence=cons, injectable=inj,
                layer=layer, found_by=found, confidence=conf, notes=notes, fix_url=fix)

def dup(of, notes=""):
    return dict(relevant="dup", fix_url=of, confidence="high", notes=notes)

def no(notes, conf="high"):
    return dict(relevant="no", confidence=conf, notes=notes)

LABELS = [
    (O, 3356, y("wrapper", "select", "data_loss", "yes", "app", "review", "high",
                "RFD 400 case: propolis recv() takes a ws message then awaits; loses it if cancelled",
                "https://github.com/oxidecomputer/omicron/pull/3411")),
    (O, 3411, dup("https://github.com/oxidecomputer/omicron/issues/3356", "fix")),
    (O, "7e979f08b9c9", dup("https://github.com/oxidecomputer/omicron/issues/3356", "fix commit")),
    (P, 434, y("wrapper", "select", "data_loss", "yes", "library", "review", "high",
               "InstanceSerialConsoleHelper::recv pulls from ws_stream then awaits",
               "https://github.com/oxidecomputer/propolis/pull/438")),
    (P, 435, dup("https://github.com/oxidecomputer/propolis/issues/434", "fix attempt")),
    (P, 438, dup("https://github.com/oxidecomputer/propolis/issues/434", "fix")),
    (P, "85d901da0d03", dup("https://github.com/oxidecomputer/propolis/issues/434", "fix commit")),
    (P, 650, y("wrapper", "select", "duplication", "yes", "app", "failure", "high",
               "send-to-listeners future recreated each loop; for_each_concurrent progress lost -> resend",
               "https://github.com/oxidecomputer/propolis/pull/830")),
    (P, 830, dup("https://github.com/oxidecomputer/propolis/issues/650", "restructuring fix (open)")),
    (O, 3579, y("context", "drop", "state_corruption", "yes", "app", "review", "medium",
                "RunningUpdate behind tokio Mutex invalid across await; later callers see it")),
    (O, "a7c7a6768cae", dup("https://github.com/oxidecomputer/omicron/pull/3579", "commit")),
    (O, 3140, y("context", "drop", "state_corruption", "yes", "app", "failure", "high",
                "request timeout cancels services_ensure; zones half-configured, retry misbehaves (#3098)")),
    (O, 3345, y("context", "drop", "state_corruption", "yes", "app", "review", "medium",
                "destructive endpoints cancelled by client disconnect; fix spawns the work")),
    (O, 9268, y("other", "select", "deadlock", "unknown", "app", "failure", "high",
                "futurelock (#9259): future holding a qorb claim in select! not polled; fix itself")),
    (O, 10204, y("context", "abort", "state_corruption", "unknown", "app", "failure", "high",
                 "task cancellation does not stop tokio::fs blocking writes; tempdir scope violated (#10063)")),
    (O, 9569, dict(relevant="unclear", confidence="low",
                   notes="sled-agent hang, fixed by #9612, cause not established from thread")),
    (O, 9590, no("preventive refactor against futurelock, no defect", "medium")),
    (O, "47b72b8018ce", no("commit of #9590", "medium")),
    (O, 9272, no("audit tracking issue for futurelock pattern", "medium")),
    (O, 10277, no("tokio regression hang, not cancellation", "medium")),
    (O, 11255, no("protocol decode error handling, not cancellation", "medium")),
    (P, 104, no("stdin-in-select mentioned as red herring; bug was terminal timing", "medium")),
    (P, 578, no("NVMe pause stuck after controller reset; not cancellation", "low")),
    (H, 3995, y("wrapper", "shutdown", "data_loss", "yes", "library", "failure", "high",
                "dispatcher dropped mid-body; body sender dropped w/o error -> silent truncation",
                "https://github.com/hyperium/hyper/pull/4016")),
    (H, 4016, dup("https://github.com/hyperium/hyper/issues/3995", "fix")),
    (H, "b7a679bad5e1", dup("https://github.com/hyperium/hyper/issues/3995", "fix commit")),
    (H, 4040, y("other", "drop", "resource_leak", "yes", "library", "failure", "high",
                "dropping h2 response future does not stop body pipe task / send RST_STREAM")),
    (H, 3199, y("wrapper", "drop", "other", "yes", "library", "failure", "medium",
                "shared connecting task owned by one request; its cancellation fails all waiters")),
    (H, 3906, y("other", "shutdown", "panic", "unknown", "library", "failure", "medium",
                "h2 debug assertion after cancellation at shutdown")),
    (H2, 907, y("other", "timeout", "panic", "yes", "library", "failure", "high",
                "debug_assert in Counts::drop fires when connection dropped with open streams")),
    (H2, 138, y("other", "drop", "resource_leak", "yes", "library", "review", "medium",
                "Stream dropped before EOS leaves stream hung")),
    (H2, 546, y("other", "shutdown", "deadlock", "unknown", "library", "failure", "medium",
                "runtime shutdown drops tasks -> deadlock; fixed in tokio#3870")),
    (H, 1716, no("missing cancellation (future not dropped on disconnect), not caused by one", "medium")),
    (H, 4017, no("feature request: RST vs FIN on cancel")),
    (H, 4119, no("error path drops oneshot senders; not future cancellation", "medium")),
    (H, 4159, no("EOF not observed; unrelated", "medium")),
    (H, 3681, no("protocol-level RST_STREAM handling, not future cancellation", "medium")),
    (H, 2723, no("user usage issue", "low")),
]
