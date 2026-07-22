import type {
	ExtensionAPI,
	ExtensionContext,
} from "@earendil-works/pi-coding-agent";
import { existsSync } from "node:fs";
import { join, resolve } from "node:path";

const HOOK_ENV = "OFFCUT_QLTY_STOP_HOOK_ACTIVE";
const REPO_ROOT = resolve(import.meta.dirname, "..", "..");

function report(
	ctx: ExtensionContext,
	message: string,
	level: "warning" | "error",
) {
	console.error(message);
	if (ctx.hasUI) {
		try {
			ctx.ui.notify(message, level);
		} catch {
			// The renderer is already torn down on the quit path; stderr carried it.
		}
	}
}

export default function (pi: ExtensionAPI) {
	pi.on("session_shutdown", async (event, ctx) => {
		if (event.reason !== "quit") {
			return;
		}

		if (process.env[HOOK_ENV]) {
			return;
		}

		const scriptPath = join(REPO_ROOT, ".qlty", "hooks", "qlty-check.py");
		if (!existsSync(scriptPath)) {
			report(
				ctx,
				"Qlty stop hook skipped: .qlty/hooks/qlty-check.py was not found",
				"warning",
			);
			return;
		}

		console.error(
			"Qlty stop hook: running qlty check, this may take a while on a cold plugin cache...",
		);

		const result = await pi.exec(
			"python3",
			[scriptPath, "--tool", "pi", "--cwd", REPO_ROOT],
			{ timeout: 600_000 },
		);

		if (result.code !== 0) {
			const output = (
				result.stderr ||
				result.stdout ||
				`qlty hook exited with code ${result.code}`
			).trim();
			report(ctx, output, "error");
		}
	});
}
