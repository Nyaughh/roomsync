package dev.roomsync;

/** Process memory only. No preferences, filesystem, backups or logs. */
final class PairingMemory {
    private PairingConfig saved;
    synchronized void store(PairingConfig config){clear();saved=config.copy();}
    synchronized PairingConfig copy(){return saved==null?null:saved.copy();}
    synchronized boolean available(){return saved!=null;}
    synchronized void clear(){if(saved!=null){saved.erase();saved=null;}}
}
