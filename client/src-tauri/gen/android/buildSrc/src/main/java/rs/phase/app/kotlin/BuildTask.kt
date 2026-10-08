import java.nio.file.Files
import java.nio.file.InvalidPathException
import java.nio.file.Path
import java.nio.file.Paths
import javax.inject.Inject
import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.RegularFileProperty
import org.gradle.api.logging.LogLevel
import org.gradle.api.model.ObjectFactory
import org.gradle.api.provider.Property
import org.gradle.api.provider.ValueSource
import org.gradle.api.provider.ValueSourceParameters
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.InputFile
import org.gradle.api.tasks.Internal
import org.gradle.api.tasks.PathSensitive
import org.gradle.api.tasks.PathSensitivity
import org.gradle.api.tasks.TaskAction
import org.gradle.process.ExecOperations

private const val TAURI_LOG_LEVEL_ERROR =
    "phaseTauriLogLevel must be one of quiet, warn, lifecycle, info, debug"

internal fun parseSelectableTauriLogLevel(value: String): LogLevel =
    when (value) {
        "quiet" -> LogLevel.QUIET
        "warn" -> LogLevel.WARN
        "lifecycle" -> LogLevel.LIFECYCLE
        "info" -> LogLevel.INFO
        "debug" -> LogLevel.DEBUG
        else -> throw GradleException(TAURI_LOG_LEVEL_ERROR)
    }

internal fun tauriVerbosityArgs(level: LogLevel): List<String> =
    when (level) {
        LogLevel.DEBUG -> listOf("-vv")
        LogLevel.INFO -> listOf("-v")
        LogLevel.QUIET,
        LogLevel.WARN,
        LogLevel.LIFECYCLE,
        LogLevel.ERROR,
        -> emptyList()
    }

abstract class NodeExecutableValueSource : ValueSource<String, NodeExecutableValueSource.Parameters> {
    interface Parameters : ValueSourceParameters {
        val path: Property<String>
        val osFamily: Property<String>
    }

    override fun obtain(): String {
        val rawPath = parameters.path.get()
        if (rawPath.isBlank()) {
            throw GradleException("phaseNodeExecutable is required")
        }

        val logicalPath =
            try {
                Paths.get(rawPath)
            } catch (_: InvalidPathException) {
                throw GradleException("phaseNodeExecutable must be absolute: $rawPath")
            }
        if (!logicalPath.isAbsolute) {
            throw GradleException("phaseNodeExecutable must be absolute: $rawPath")
        }

        val realPath = logicalPath.requireRealPath("phaseNodeExecutable")
        if (!Files.isRegularFile(realPath)) {
            throw GradleException("phaseNodeExecutable must be a regular file: $rawPath")
        }

        when (parameters.osFamily.get()) {
            "windows" -> {
                if (!realPath.fileName.toString().endsWith(".exe", ignoreCase = true)) {
                    throw GradleException(
                        "phaseNodeExecutable must resolve to an .exe on Windows: $realPath"
                    )
                }
            }
            "posix" -> {
                if (!Files.isExecutable(realPath)) {
                    throw GradleException(
                        "phaseNodeExecutable must be executable on POSIX: $realPath"
                    )
                }
            }
            else -> throw GradleException("unsupported normalized OS family")
        }

        return realPath.toString()
    }
}

abstract class TauriCliScriptValueSource : ValueSource<String, TauriCliScriptValueSource.Parameters> {
    interface Parameters : ValueSourceParameters {
        val logicalPath: Property<String>
        val clientRoot: Property<String>
        val osFamily: Property<String>
    }

    override fun obtain(): String {
        val clientRoot = Paths.get(parameters.clientRoot.get()).toRealPath()
        val logicalPath = Paths.get(parameters.logicalPath.get()).toAbsolutePath().normalize()
        val expectedPath =
            clientRoot.resolve("node_modules/@tauri-apps/cli/tauri.js").normalize()
        if (!logicalPath.equalsForOs(expectedPath, parameters.osFamily.get())) {
            throw GradleException(
                "tauriCliScript must be the lock-installed tauri.js: $logicalPath"
            )
        }

        val realPath = logicalPath.requireRealPath("tauriCliScript")
        if (!Files.isRegularFile(realPath)) {
            throw GradleException("tauriCliScript must be a regular file: $logicalPath")
        }
        if (!realPath.startsWith(clientRoot)) {
            throw GradleException("tauriCliScript escapes the client directory: $realPath")
        }

        return logicalPath.toString()
    }
}

private fun Path.requireRealPath(propertyName: String): Path =
    try {
        toRealPath()
    } catch (_: Exception) {
        throw GradleException("$propertyName does not exist: $this")
    }

private fun Path.equalsForOs(other: Path, osFamily: String): Boolean =
    when (osFamily) {
        "windows" -> toString().equals(other.toString(), ignoreCase = true)
        "posix" -> this == other
        else -> throw GradleException("unsupported normalized OS family")
    }

abstract class BuildTask @Inject constructor(
    private val execOperations: ExecOperations,
    objects: ObjectFactory,
) : DefaultTask() {
    @get:Input
    abstract val nodeExecutable: Property<String>

    @get:InputFile
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val tauriCliScript: RegularFileProperty

    @get:Input
    abstract val tauriLogLevel: Property<String>

    @get:Input
    abstract val rootDirRel: Property<String>

    @get:Input
    abstract val target: Property<String>

    @get:Input
    abstract val release: Property<Boolean>

    @get:Internal
    val workingDirectory: DirectoryProperty = objects.directoryProperty()

    @TaskAction
    fun assemble() {
        val arguments =
            buildList {
                add(tauriCliScript.get().asFile.absolutePath)
                add("android")
                add("android-studio-script")
                addAll(tauriVerbosityArgs(parseSelectableTauriLogLevel(tauriLogLevel.get())))
                add("--target")
                add(target.get())
                if (release.get()) {
                    add("--release")
                }
            }

        execOperations
            .exec {
                workingDir(workingDirectory.get().asFile)
                executable(nodeExecutable.get())
                args(arguments)
            }
            .assertNormalExitValue()
    }
}
