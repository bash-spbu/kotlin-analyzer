-- :KotlinGradle build|clean|test [project] — Gradle for the current file's
-- build, run in a terminal split by editors/gradle/gradle_task.py:
--
--   build   gradle build -x test
--   clean   gradle clean
--   test    gradle test --rerun-tasks in the file's project and its
--           subprojects, or in [project]: the file's project or a parent,
--           by Gradle path (completed with <Tab>)

local gradle = {}

local TASKS = { "build", "clean", "test" }

--- gradle_task.py, next to this plugin in the repository.
local function script()
  local this_file = debug.getinfo(1, "S").source:sub(2)
  local plugin = vim.fs.dirname(vim.fs.dirname(vim.fs.dirname(this_file)))
  return vim.fs.joinpath(vim.fs.dirname(plugin), "gradle", "gradle_task.py")
end

local function current_path()
  local file = vim.api.nvim_buf_get_name(0)
  if file == "" then
    return vim.fn.getcwd()
  end
  return file
end

--- The projects holding the current file, nearest first: { path, directory }.
---@return { path: string, directory: string }[]
function gradle.projects()
  local result = vim.system({ "python3", "-I", script(), "projects", current_path() }, { text = true }):wait()
  if result.code ~= 0 then
    return {}
  end
  return vim.json.decode(result.stdout)
end

local function run_in_terminal(arguments)
  vim.cmd("botright 15split")
  vim.cmd.enew()
  vim.fn.jobstart(vim.list_extend({ "python3", "-I", script() }, arguments), { term = true })
  vim.cmd.startinsert()
end

--- Runs the tests `filter` names (Gradle's `--tests`) in the project
--- holding `path`: what a "Run test" lens asks for.
function gradle.run_tests(path, filter)
  run_in_terminal({ "tests", path, filter })
end

local function command(invocation)
  local task, project = invocation.fargs[1], invocation.fargs[2]
  if not vim.list_contains(TASKS, task) then
    vim.notify("KotlinGradle: build, clean or test", vim.log.levels.ERROR)
    return
  end
  if project == nil then
    run_in_terminal({ task, current_path() })
    return
  end
  for _, candidate in ipairs(gradle.projects()) do
    if candidate.path == project then
      run_in_terminal({ task, candidate.directory })
      return
    end
  end
  vim.notify("KotlinGradle: " .. project .. " doesn't hold this file", vim.log.levels.ERROR)
end

local function complete(_, line)
  local words = vim.split(line, "%s+", { trimempty = false })
  if #words <= 2 then
    return TASKS
  end
  if words[2] ~= "test" or #words > 3 then
    return {}
  end
  return vim.tbl_map(function(project)
    return project.path
  end, gradle.projects())
end

function gradle.create_command()
  vim.api.nvim_create_user_command("KotlinGradle", command, {
    nargs = "+",
    complete = complete,
    desc = "Gradle build -x test / clean / test --rerun-tasks for the current file's build",
  })
end

return gradle
