--- jumanji.nvim — two-way sync between Neovim and the jumanji markdown reader.
---
--- Forward (editor → reader): `open()` pairs the current buffer with a reader —
--- reusing a running instance that already has the file open (found over the
--- session bus, like jumanji's own `--forward` routing) or spawning one at the
--- cursor line. While the paired reader is alive, CursorHold/BufWritePost keep
--- it following the cursor; when it exits, sync disarms silently.
---
--- Reverse (reader → editor): jumanji's `editor-command` runs the sibling
--- `reverse.lua` (`nvim -l …/reverse.lua %l %f`), which RPCs into every running
--- Neovim; `inverse_search()` is the receiving end — the instance that has the
--- file loaded claims the jump.

local M = {}

M.config = {
  -- The reader executable.
  cmd = "jumanji",
  -- Keep the reader following the cursor (CursorHold/BufWritePost) while the
  -- paired instance is open. `false` limits forward sync to explicit `open()`.
  live = true,
}

local uv = vim.uv or vim.loop

--- Per-buffer sync state: pid of the paired reader + last line pushed.
---@type table<integer, {pid: integer, last_line: integer?}>
local state = {}

local function alive(pid)
  return uv.kill(pid, 0) == 0
end

local function realpath(path)
  return uv.fs_realpath(path) or path
end

local BUS = "org.membranepotential.jumanji"
local OBJECT_PATH = "/org/membranepotential/jumanji"

--- Run one `gdbus call --session …` synchronously; nil on any failure.
local function gdbus(args)
  local cmd = vim.list_extend({ "gdbus", "call", "--session" }, args)
  local ok, proc = pcall(vim.system, cmd, { text = true, timeout = 3000 })
  if not ok then
    return nil
  end
  local out = proc:wait()
  return out.code == 0 and out.stdout or nil
end

--- Find a running jumanji instance that has `file` open; returns its pid.
--- Mirrors jumanji's forward routing: enumerate `<BUS>.PID-<pid>` names on the
--- session bus and ask each `GetState` for its file. The pid (embedded in the
--- bus name) is what lets live sync track an instance we did not spawn.
local function find_instance(file)
  if vim.fn.executable("gdbus") == 0 then
    return nil
  end
  local names = gdbus({
    "--dest", "org.freedesktop.DBus",
    "--object-path", "/org/freedesktop/DBus",
    "--method", "org.freedesktop.DBus.ListNames",
  })
  if not names then
    return nil
  end
  local target = realpath(file)
  for pid in names:gmatch("org%.membranepotential%.jumanji%.PID%-(%d+)") do
    local st = gdbus({
      "--dest", BUS .. ".PID-" .. pid,
      "--object-path", OBJECT_PATH,
      "--method", BUS .. ".GetState",
    })
    local open = st and st:match('"file":"([^"]*)"')
    if open and realpath(open) == target then
      return tonumber(pid)
    end
  end
  return nil
end

--- Fire-and-forget `jumanji --forward <line> <file>` — routes to the running
--- instance over D-Bus and exits (never opens a second window for an open file).
local function push(file, line)
  pcall(vim.system, { M.config.cmd, "--forward", tostring(line), file }, {}, function() end)
end

--- Push the cursor line of `buf` to its paired reader, if one is alive.
--- Deduplicates by line unless `force` (a save re-anchors after reload).
function M.forward(buf, force)
  buf = buf or vim.api.nvim_get_current_buf()
  local s = state[buf]
  if not s then
    return
  end
  if not alive(s.pid) then
    state[buf] = nil -- reader closed: disarm quietly, `open()` re-pairs
    return
  end
  if vim.api.nvim_get_current_buf() ~= buf then
    return
  end
  local line = vim.api.nvim_win_get_cursor(0)[1]
  if not force and s.last_line == line then
    return
  end
  s.last_line = line
  push(vim.api.nvim_buf_get_name(buf), line)
end

--- Pair the current buffer with a reader and jump it to the cursor line:
--- reuse the instance that already has the file open, else spawn one
--- (detached — the reader outlives this Neovim).
function M.open()
  local buf = vim.api.nvim_get_current_buf()
  local file = vim.api.nvim_buf_get_name(buf)
  if file == "" then
    vim.notify("jumanji: buffer has no file", vim.log.levels.WARN)
    return
  end
  local s = state[buf]
  if s and alive(s.pid) then
    M.forward(buf, true)
    return
  end
  local pid = find_instance(file)
  if pid then
    state[buf] = { pid = pid }
    M.forward(buf, true)
    return
  end
  local line = vim.api.nvim_win_get_cursor(0)[1]
  local ok, proc =
    pcall(vim.system, { M.config.cmd, "--forward", tostring(line), file }, { detach = true }, function() end)
  if not ok or not proc.pid then
    vim.notify("jumanji: failed to launch `" .. M.config.cmd .. "`", vim.log.levels.ERROR)
    return
  end
  state[buf] = { pid = proc.pid, last_line = line }
end

--- Raise the terminal window this Neovim lives in (reverse sync lands the
--- cursor, this lands the eyes). Best-effort: X11 + $WINDOWID + xdotool.
local function focus_terminal()
  local id = vim.env.WINDOWID
  if id and vim.fn.executable("xdotool") == 1 then
    pcall(vim.system, { "xdotool", "windowactivate", id }, {}, function() end)
  end
end

--- Receiving end of reverse sync (called over RPC by reverse.lua): if this
--- instance has `file` loaded, show it, jump to `line`, raise the terminal.
--- Returns true iff the jump was claimed.
function M.inverse_search(line, file)
  local target = realpath(file)
  for _, buf in ipairs(vim.api.nvim_list_bufs()) do
    if vim.api.nvim_buf_is_loaded(buf) and vim.bo[buf].buftype == "" then
      local name = vim.api.nvim_buf_get_name(buf)
      if name ~= "" and realpath(name) == target then
        if vim.fn.mode():sub(1, 1) == "i" then
          vim.cmd.stopinsert()
        end
        local win = vim.fn.win_findbuf(buf)[1]
        if win then
          vim.fn.win_gotoid(win) -- switches tabpage too
        else
          vim.api.nvim_win_set_buf(0, buf)
        end
        line = math.max(1, math.min(line, vim.api.nvim_buf_line_count(buf)))
        vim.api.nvim_win_set_cursor(0, { line, 0 })
        vim.cmd("normal! zv")
        focus_terminal()
        return true
      end
    end
  end
  return false
end

function M.setup(opts)
  M.config = vim.tbl_deep_extend("force", M.config, opts or {})
  local group = vim.api.nvim_create_augroup("jumanji_sync", { clear = true })
  vim.api.nvim_create_autocmd({ "CursorHold", "BufWritePost" }, {
    group = group,
    desc = "jumanji: forward-sync cursor to the paired reader",
    callback = function(ev)
      if M.config.live and state[ev.buf] then
        M.forward(ev.buf, ev.event == "BufWritePost")
      end
    end,
  })
  vim.api.nvim_create_autocmd("BufWipeout", {
    group = group,
    callback = function(ev)
      state[ev.buf] = nil
    end,
  })
  vim.api.nvim_create_user_command("Jumanji", M.open, {
    desc = "Open/sync the jumanji reader for this buffer",
  })
end

return M
