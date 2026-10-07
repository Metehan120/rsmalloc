/*
 * Regression for allocating atfork callbacks registered before rsmalloc's
 * lazy handler registration. Use dlopen so libc startup cannot initialize it.
 *
 * cargo build --release --features preload
 * cc -O2 -pthread tests/preload_fork.c -ldl -o target/preload-fork
 * timeout 30s target/preload-fork ./target/release/librsmalloc.so
 */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

static void *(*rs_malloc)(size_t);
static void *(*rs_realloc)(void *, size_t);
static void (*rs_free)(void *);
static int (*rs_trim)(size_t);
static _Atomic unsigned prepare_calls;
static _Atomic unsigned parent_calls;
static _Atomic unsigned child_calls;
static _Atomic int multithreaded;

static void require(int condition, int code) {
    if (!condition)
        _exit(code);
}

static void exercise_allocator(void) {
    unsigned char *small[1024];
    for (size_t i = 0; i < 1024; ++i) {
        small[i] = rs_malloc(1024);
        require(small[i] != NULL, 10);
        small[i][0] = 0x29;
        small[i][1023] = 0x83;
    }
    for (size_t i = 0; i < 1024; ++i) {
        require(small[i][0] == 0x29 && small[i][1023] == 0x83, 11);
        rs_free(small[i]);
    }

    /* Exceed the initial segmented region and borrow its growth reservation. */
    void *segmented[10];
    for (size_t i = 0; i < 10; ++i) {
        segmented[i] = rs_malloc(8 * 1024 * 1024);
        require(segmented[i] != NULL, 12);
        ((unsigned char *)segmented[i])[0] = 0x51;
    }
    for (size_t i = 0; i < 10; ++i) {
        require(((unsigned char *)segmented[i])[0] == 0x51, 13);
        rs_free(segmented[i]);
    }

    /* Also cover direct mappings and the sharded big-allocation metadata. */
    const size_t old_size = 80 * 1024 * 1024;
    unsigned char *direct = rs_malloc(old_size);
    require(direct != NULL, 14);
    direct[0] = 0xa5;
    direct[old_size - 1] = 0x5a;
    unsigned char *grown = rs_realloc(direct, 96 * 1024 * 1024);
    require(grown != NULL, 15);
    require(grown[0] == 0xa5 && grown[old_size - 1] == 0x5a, 16);
    rs_free(grown);

    /* Trimming may decline while prepare holds the global trim lock. */
    (void)rs_trim(0);
}

static void older_prepare(void) {
    exercise_allocator();
    atomic_fetch_add(&prepare_calls, 1);
}

static void older_parent(void) {
    exercise_allocator();
    atomic_fetch_add(&parent_calls, 1);
}

static void older_child(void) {
    /* Allocation is permitted in the single-threaded fork cases only. */
    if (!atomic_load(&multithreaded))
        exercise_allocator();
    atomic_fetch_add(&child_calls, 1);
}

static void run_fork(void) {
    pid_t child = fork();
    require(child >= 0, 20);
    if (child == 0) {
        require(atomic_load(&child_calls) == 1, 21);
        if (!atomic_load(&multithreaded))
            exercise_allocator();
        _exit(0);
    }
    int status;
    require(waitpid(child, &status, 0) == child, 22);
    require(WIFEXITED(status) && WEXITSTATUS(status) == 0, 23);
    exercise_allocator();
}

static void *fork_worker(void *unused) {
    (void)unused;
    for (int i = 0; i < 3; ++i)
        run_fork();
    return NULL;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: %s path/to/librsmalloc.so\n", argv[0]);
        return 1;
    }
    require(setenv("RS_ARENA_SIZE", "262144", 1) == 0, 30);
    require(setenv("RS_DISABLE_TRIM_THREAD", "1", 1) == 0, 31);
    require(setenv("RS_TRIMMER_THRESHOLD", "18446744073709551615", 1) == 0, 32);
    require(setenv("RS_BIG_TRIMMER_THRESHOLD", "18446744073709551615", 1) == 0, 33);
    require(pthread_atfork(older_prepare, older_parent, older_child) == 0, 34);

    void *library = dlopen(argv[1], RTLD_NOW | RTLD_LOCAL);
    if (library == NULL) {
        fprintf(stderr, "dlopen: %s\n", dlerror());
        return 1;
    }
    rs_malloc = dlsym(library, "malloc");
    rs_realloc = dlsym(library, "realloc");
    rs_free = dlsym(library, "free");
    rs_trim = dlsym(library, "malloc_trim");
    require(rs_malloc && rs_realloc && rs_free && rs_trim, 35);

    void *initial = rs_malloc(16);
    require(initial != NULL, 36);
    rs_free(initial);

    for (int i = 0; i < 4; ++i)
        run_fork();

    atomic_store(&multithreaded, 1);
    pthread_t workers[2];
    for (int i = 0; i < 2; ++i)
        require(pthread_create(&workers[i], NULL, fork_worker, NULL) == 0, 37);
    for (int i = 0; i < 2; ++i)
        require(pthread_join(workers[i], NULL) == 0, 38);
    require(atomic_load(&prepare_calls) == 10, 39);
    require(atomic_load(&parent_calls) == 10, 40);
    puts("preload fork regression passed: allocating prepare/parent/child callbacks and concurrent forks");
    /* Keep the library loaded: its registered handlers have process lifetime. */
    return 0;
}
