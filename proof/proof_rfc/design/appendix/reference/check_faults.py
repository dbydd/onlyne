"""汇总 proof/proof_rfc/design/appendix/reference 下各单点状态搜索的结果。

每个模块暴露 run() -> list[dict]，字段见各脚本。本脚本只做汇总与退出码，
不做额外断言。用法：python3 proof/proof_rfc/design/appendix/reference/check_faults.py
"""

import importlib.util
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent

MODULES = [
    "admission_safety",
    "handover_deadlines",
    "timeout_regression",
    "grant_transaction_race",
    "prepare_recovery_progress",
    "attention_feed_race",
    "ack_authority_race",
    "backlog_expiry_liveness",
]


def load(name):
    path = HERE / f"{name}.py"
    if not path.exists():
        return None
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


def main():
    report = {"scenarios": [], "all_checks_pass": True}
    for name in MODULES:
        module = load(name)
        if module is None:
            report["scenarios"].append(
                {
                    "module": name,
                    "error": "missing script",
                    "all_checks_pass": False,
                }
            )
            report["all_checks_pass"] = False
            continue
        try:
            results = module.run()
        except Exception as exc:  # 保留失败模块，不让汇总静默跳过
            report["scenarios"].append(
                {"module": name, "error": f"{type(exc).__name__}: {exc}", "all_checks_pass": False}
            )
            report["all_checks_pass"] = False
            continue
        passed = all(item.get("check_pass", False) for item in results)
        report["scenarios"].append({"module": name, "results": results, "all_checks_pass": passed})
        report["all_checks_pass"] = report["all_checks_pass"] and passed

    print(json.dumps(report, ensure_ascii=False, indent=2))
    return 0 if report["all_checks_pass"] else 1


if __name__ == "__main__":
    sys.exit(main())
