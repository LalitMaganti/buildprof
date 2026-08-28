#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static void sleep_ms(long milliseconds) {
    struct timespec delay = {
        .tv_sec = milliseconds / 1000,
        .tv_nsec = (milliseconds % 1000) * 1000000,
    };
    while (nanosleep(&delay, &delay) == -1 && errno == EINTR) {}
}

static int wait_for(pid_t pid) {
    int status;
    while (waitpid(pid, &status, 0) == -1) {
        if (errno != EINTR) return 111;
    }
    if (WIFEXITED(status)) return WEXITSTATUS(status);
    if (WIFSIGNALED(status)) return 128 + WTERMSIG(status);
    return 112;
}

static void self_path(char *buffer, size_t size) {
    ssize_t length = readlink("/proc/self/exe", buffer, size - 1);
    if (length < 0 || (size_t)length >= size - 1) {
        perror("readlink /proc/self/exe");
        exit(113);
    }
    buffer[length] = '\0';
}

int main(int argc, char **argv) {
    if (argc < 2) return 64;

    if (!strcmp(argv[1], "single") || !strcmp(argv[1], "leaf") ||
        !strcmp(argv[1], "exec-leaf")) {
        sleep_ms(20);
        return 0;
    }

    if (!strcmp(argv[1], "exit")) {
        if (argc != 3) return 64;
        return atoi(argv[2]);
    }

    if (!strcmp(argv[1], "open-file")) {
        if (argc != 3) return 64;
        int fd = open(argv[2], O_RDONLY);
        if (fd < 0) return 115;
        char byte;
        ssize_t count = read(fd, &byte, sizeof(byte));
        if (close(fd) != 0 || count < 0) return 116;
        return 0;
    }

    if (!strcmp(argv[1], "output")) {
        static const char stdout_text[] = "fixture stdout\n";
        static const char stderr_text[] = "fixture stderr\n";
        write(STDOUT_FILENO, stdout_text, sizeof(stdout_text) - 1);
        write(STDERR_FILENO, stderr_text, sizeof(stderr_text) - 1);
        return 0;
    }

    if (!strcmp(argv[1], "fork-exec")) {
        char executable[4096];
        self_path(executable, sizeof(executable));
        pid_t child = fork();
        if (child < 0) return 114;
        if (child == 0) {
            execl(executable, executable, "leaf", NULL);
            _exit(127);
        }
        return wait_for(child);
    }

    if (!strcmp(argv[1], "fork-no-exec")) {
        pid_t child = fork();
        if (child < 0) return 114;
        if (child == 0) {
            sleep_ms(30);
            _exit(0);
        }
        return wait_for(child);
    }

    if (!strcmp(argv[1], "exec-chain")) {
        char executable[4096];
        self_path(executable, sizeof(executable));
        execl(executable, executable, "exec-leaf", NULL);
        return 127;
    }

    if (!strcmp(argv[1], "parallel")) {
        if (argc != 3) return 64;
        int count = atoi(argv[2]);
        if (count < 1 || count > 64) return 64;
        pid_t children[64];
        for (int i = 0; i < count; ++i) {
            children[i] = fork();
            if (children[i] < 0) return 114;
            if (children[i] == 0) {
                sleep_ms(100);
                _exit(0);
            }
        }
        int result = 0;
        for (int i = 0; i < count; ++i) {
            int child_result = wait_for(children[i]);
            if (child_result && !result) result = child_result;
        }
        return result;
    }

    fprintf(stderr, "unknown fixture mode: %s\n", argv[1]);
    return 64;
}
