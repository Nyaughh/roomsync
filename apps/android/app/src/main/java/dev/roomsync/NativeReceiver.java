package dev.roomsync;

final class NativeReceiver {
    static { System.loadLibrary("roomsync_android"); }
    private NativeReceiver() {}
    static native long start(String host, int hostPort, int localPort, char[] key, int channel);
    static native int phase(long handle);
    static native String status(long handle);
    static native void render(long handle, long localDacNs, int frames, float[] output);
    static native void cancel(long handle);
    static native void stop(long handle);
}
