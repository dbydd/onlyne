--!strict
-- The window half: palette commands and keys that open the Onlyne Board
-- beside the focused pane (or focus the one already open in this window),
-- plus a command that adds the focused pane's nearest directory as a
-- watched server root. The block itself lives on the host: the board runs
-- `onlyne` there, where the runtime directory and the daemons are.

local KIND = "onlyne.board"

local function open_board(cx: WindowCx, args: { string }?)
	-- One board per window is enough: every board shares the host's poll loop.
	for _, pane in cx.session:panes() do
		if pane.kind == "plugin" and pane.block == KIND then
			cx.layout:focus(pane.pane)
			return
		end
	end
	if cx:new_block(KIND, args, "beside") == nil then
		cx:toast("error", "Onlyne Board is not loaded on this host", "Install the plugin where your panes run.")
	end
end

tern.command({
	id = "open",
	title = "Open Onlyne board",
	icon = "radar",
	run = function(cx: WindowCx)
		open_board(cx, nil)
	end,
})

tern.command({
	id = "watch-here",
	title = "Watch this directory as an Onlyne server root",
	icon = "cluster",
	run = function(cx: WindowCx)
		local focused = cx.session:focused()
		local cwd: string? = nil
		for _, pane in cx.session:panes() do
			if pane.pane == focused then
				cwd = pane.cwd
				break
			end
		end
		if cwd == nil or (cwd :: string) == "" then
			cx:toast("error", "No directory to watch", "The focused pane has no working directory.")
			return
		end
		open_board(cx, { cwd :: string })
	end,
})

tern.bind(if tern.runtime.os == "macos" then "cmd+alt+shift+o" else "ctrl+alt+shift+o", "plugin.onlyne.open")
