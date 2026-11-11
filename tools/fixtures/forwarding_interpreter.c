/* A disposable script interpreter whose native parent remains alive while the
 * Python LSP peer runs. This permits safe owned-image replacement tests without
 * relocating a platform-signed system executable. */
#include <errno.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

int main(int argc, char **argv) {
    if (argc < 3) return 125;
    pid_t child = fork();
    if (child < 0) return 124;
    if (child == 0) {
        execv(argv[1], argv + 1);
        _exit(127);
    }
    int status;
    while (waitpid(child, &status, 0) < 0) {
        if (errno != EINTR) return 123;
    }
    return WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status);
}
