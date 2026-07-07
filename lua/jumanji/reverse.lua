--- Reverse editor sync entry point (jumanji Ctrl+click → running Neovim).
---
--- Wired via jumanji's `editor-command`:
---
---   editor-command = "nvim -l /abs/path/to/lua/jumanji/reverse.lua %l %f"
---
--- Runs as a short-lived scripting-mode Neovim (vimtex's inverse-search
--- pattern): find every running instance through its default server socket
--- (`stdpath("run")/nvim.<pid>.0`), ask each over RPC to claim the jump — an
--- instance claims it when it has the file loaded (see `init.lua`
--- `inverse_search`). If nobody claims it, fall back to opening the file in the
--- first reachable instance; with no instance at all, exit nonzero (jumanji
--- treats reverse sync as best-effort).

local line = tonumber(arg and arg[1])
local file = arg and arg[2]
if not line or not file then
  io.stderr:write("usage: nvim -l reverse.lua <line> <file>\n")
  os.exit(2)
end

-- Claims only when the plugin is loaded there — which is exactly when that
-- instance can own a markdown buffer (lazy-loaded on ft=markdown).
local claim = [[
  local line, file = ...
  local ok, jumanji = pcall(require, "jumanji")
  return ok and jumanji.inverse_search(line, file) == true
]]

-- Self-contained (the target may not have the plugin loaded): open the file at
-- the line in the instance's current window, then raise its terminal.
local open_fallback = [[
  local line, file = ...
  if vim.fn.filereadable(file) == 0 then return false end
  if vim.fn.mode():sub(1, 1) == "i" then vim.cmd.stopinsert() end
  local ok = pcall(vim.cmd, ("edit +%d %s"):format(line, vim.fn.fnameescape(file)))
  if ok and vim.env.WINDOWID and vim.fn.executable("xdotool") == 1 then
    vim.system({ "xdotool", "windowactivate", vim.env.WINDOWID })
  end
  return ok
]]

--- Execute `code` with (line, file) in the instance behind `sock`.
--- Returns its boolean result, or nil if the instance was unreachable.
local function rpc(sock, code)
  local ok, chan = pcall(vim.fn.sockconnect, "pipe", sock, { rpc = true })
  if not ok or chan == 0 then
    return nil
  end
  local ok2, res = pcall(vim.rpcrequest, chan, "nvim_exec_lua", code, { line, file })
  pcall(vim.fn.chanclose, chan)
  if not ok2 then
    return nil
  end
  return res -- false is meaningful: reachable but did not claim
end

-- Default server sockets are `nvim.<pid>.<counter>`; match on the socket file
-- type rather than the exact shape, but always skip our own pid — even this
-- scripting-mode process owns a default socket, and RPCing ourselves deadlocks
-- (v:servername is unreliable here, so the pid is the guard).
local uv = vim.uv or vim.loop
local own_pid = uv.os_getpid()
local sockets = {}
for _, sock in ipairs(vim.fn.glob(vim.fn.stdpath("run") .. "/nvim.*", true, true)) do
  local st = uv.fs_stat(sock)
  local pid = tonumber(vim.fs.basename(sock):match("^nvim%.(%d+)"))
  if st and st.type == "socket" and pid ~= own_pid then
    table.insert(sockets, sock)
  end
end

local reachable = {}
for _, sock in ipairs(sockets) do
  local res = rpc(sock, claim)
  if res == true then
    os.exit(0)
  end
  if res == false then
    table.insert(reachable, sock)
  end
end

for _, sock in ipairs(reachable) do
  if rpc(sock, open_fallback) == true then
    os.exit(0)
  end
end
os.exit(1)
