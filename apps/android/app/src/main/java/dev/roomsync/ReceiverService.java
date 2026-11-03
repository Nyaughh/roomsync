package dev.roomsync;

import android.app.*;
import android.content.*;
import android.content.pm.ServiceInfo;
import android.media.*;
import android.os.*;
import java.util.Arrays;

public final class ReceiverService extends Service {
    static volatile String status="Ready. Start the Windows host, then connect here.";
    static volatile boolean running;
    private static PairingConfig pending;
    static final PairingMemory pairing=new PairingMemory();
    private static final String START="dev.roomsync.START", STOP="dev.roomsync.STOP", CHANNEL="playback";
    private static final int NOTIFICATION=7;
    private volatile boolean cancelled;
    private volatile String stopReason="Stopped";
    private volatile long nativeHandle;
    private volatile AudioTrack output;
    private Thread worker;
    private AudioManager manager;
    private AudioFocusRequest focus;
    private final FocusState focusState=new FocusState();
    private PowerManager.WakeLock wakeLock;
    private boolean noisyRegistered;
    private final Handler main=new Handler(Looper.getMainLooper());
    private int routeId=-1;
    private final Runnable focusTimeout=new Runnable(){@Override public void run(){
        if(cancelled||focusState.volume()!=0f)return;
        long now=System.nanoTime();
        if(focusState.expired(now))cancel("Audio focus did not return within 30 seconds. Reconnect when the other app finishes.");
        else main.postDelayed(this,Math.max(1,focusState.timeoutDelayMs(now))); // uptime/nanoTime rounding may fire early.
    }};
    private final BroadcastReceiver noisy=new BroadcastReceiver(){@Override public void onReceive(Context c,Intent i){cancel("Audio device disconnected. Reconnect to recalibrate.");}};

    static synchronized void begin(Context c,PairingConfig config) {
        if(running){config.erase();throw new IllegalStateException("Receiver is already running or stopping");}
        pairing.store(config);pending=config; running=true; status="Starting encrypted receiver…";
        try { c.startForegroundService(new Intent(c,ReceiverService.class).setAction(START)); }
        catch(RuntimeException e){pending=null;config.erase();running=false;status="Unable to start: "+e.getMessage();throw e;}
    }
    static void requestStop(Context c,String reason){if(running)c.startService(new Intent(c,ReceiverService.class).setAction(STOP).putExtra("reason",reason));}
    private static synchronized PairingConfig takeConfig(){PairingConfig c=pending;pending=null;return c;}
    @Override public void onCreate(){super.onCreate();manager=(AudioManager)getSystemService(AUDIO_SERVICE);NotificationManager n=(NotificationManager)getSystemService(NOTIFICATION_SERVICE);n.createNotificationChannel(new NotificationChannel(CHANNEL,"RoomSync playback",NotificationManager.IMPORTANCE_LOW));}
    @Override public int onStartCommand(Intent intent,int flags,int id){
        if(intent==null){stopSelf();return START_NOT_STICKY;}
        if(STOP.equals(intent.getAction())){cancel(intent.getStringExtra("reason") == null ? "Stopped from notification" : intent.getStringExtra("reason"));return START_NOT_STICKY;}
        if(worker!=null)return START_NOT_STICKY;
        PairingConfig config=takeConfig();
        if(config==null){running=false;stopSelf();return START_NOT_STICKY;}
        cancelled=false;stopReason="Stopped";routeId=-1;focusState.gain();
        try {
            Notification notification=notification();
            if(Build.VERSION.SDK_INT>=29)startForeground(NOTIFICATION,notification,ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);else startForeground(NOTIFICATION,notification);
            AudioAttributes attributes=new AudioAttributes.Builder().setUsage(AudioAttributes.USAGE_MEDIA).setContentType(AudioAttributes.CONTENT_TYPE_MUSIC).build();
            focus=new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN).setAudioAttributes(attributes).setAcceptsDelayedFocusGain(false).setOnAudioFocusChangeListener(this::onFocusChange,main).build();
            if(manager.requestAudioFocus(focus)!=AudioManager.AUDIOFOCUS_REQUEST_GRANTED)throw new IllegalStateException("Audio focus is unavailable");
            if(Build.VERSION.SDK_INT>=33)registerReceiver(noisy,new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY),Context.RECEIVER_NOT_EXPORTED);else registerReceiver(noisy,new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY));
            noisyRegistered=true;
            wakeLock=((PowerManager)getSystemService(POWER_SERVICE)).newWakeLock(PowerManager.PARTIAL_WAKE_LOCK,"RoomSync:Playback");wakeLock.acquire();
            worker=new Thread(()->receive(config,attributes),"roomsync-playback");worker.start();
        }catch(Exception e){config.erase();status="Stopped: "+e.getMessage();running=false;cleanup();stopSelf();}
        return START_NOT_STICKY;
    }
    static String diagnostics(){return "RoomSync Android 0.1.3 (4)\nAndroid "+Build.VERSION.RELEASE+" / API "+Build.VERSION.SDK_INT+"\nLocal IPv4 addresses:\n"+LocalAddresses.describe()+"\n\n"+status+"\nNo pairing key is included.";}
    private Notification notification(){
        PendingIntent open=PendingIntent.getActivity(this,0,new Intent(this,MainActivity.class),PendingIntent.FLAG_IMMUTABLE|PendingIntent.FLAG_UPDATE_CURRENT);
        PendingIntent stop=PendingIntent.getService(this,1,new Intent(this,ReceiverService.class).setAction(STOP),PendingIntent.FLAG_IMMUTABLE|PendingIntent.FLAG_UPDATE_CURRENT);
        return new Notification.Builder(this,CHANNEL).setSmallIcon(dev.roomsync.R.drawable.ic_roomsync).setContentTitle("RoomSync receiver").setContentText("Encrypted audio receiver is active. Tap to view status.").setContentIntent(open).addAction(new Notification.Action.Builder(null,"Stop",stop).build()).setOngoing(true).setOnlyAlertOnce(true).setCategory(Notification.CATEGORY_TRANSPORT).build();
    }
    private void receive(PairingConfig config,AudioAttributes attributes){
        android.os.Process.setThreadPriority(android.os.Process.THREAD_PRIORITY_AUDIO);
        AudioTrack track=null;
        AudioRouting.OnRoutingChangedListener routeListener=null;
        try {
            nativeHandle=NativeReceiver.start(config.host,config.hostPort,config.localPort,config.key,config.channel);config.erase();
            long timelineStarted=0;
            while(!cancelled&&NativeReceiver.phase(nativeHandle)==1){status=NativeReceiver.status(nativeHandle);Thread.sleep(50);}
            if(cancelled)return;
            if(NativeReceiver.phase(nativeHandle)<0)throw new IllegalStateException(NativeReceiver.status(nativeHandle));
            int min=AudioTrack.getMinBufferSize(48_000,AudioFormat.CHANNEL_OUT_STEREO,AudioFormat.ENCODING_PCM_FLOAT);
            if(min<=0||min>48_000*8/5)throw new IllegalStateException("This output does not support a bounded 48 kHz float AudioTrack");
            int bytes=Math.max(min,48_000*8/25); // At least 40 ms, never unbounded.
            track=new AudioTrack.Builder().setAudioAttributes(attributes).setAudioFormat(new AudioFormat.Builder().setSampleRate(48_000).setEncoding(AudioFormat.ENCODING_PCM_FLOAT).setChannelMask(AudioFormat.CHANNEL_OUT_STEREO).build()).setTransferMode(AudioTrack.MODE_STREAM).setBufferSizeInBytes(bytes).setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY).build();
            if(track.getState()!=AudioTrack.STATE_INITIALIZED)throw new IllegalStateException("AudioTrack initialization failed");
            final AudioTrack routed=track;
            routeListener=router->{if(output!=routed||cancelled)return;AudioDeviceInfo device=routed.getRoutedDevice();if(device==null)return;int next=device.getId();if(routeId>=0&&next!=routeId)cancel("Output route changed. Reconnect to recalibrate.");routeId=next;};
            track.addOnRoutingChangedListener(routeListener,main);
            output=track;track.setVolume(focusState.volume());track.play();
            Timing timing=new Timing();OutputBudget budget=new OutputBudget();AudioTimestamp stamp=new AudioTimestamp();float[] block=new float[480*2];
            long written=0, nextStatus=0, calibrationStarted=System.nanoTime();int lastUnderruns=track.getUnderrunCount();boolean hadTiming=false;
            while(!cancelled){
                if(focusState.expired(System.nanoTime()))throw new IllegalStateException("Audio focus did not return within 30 seconds. Reconnect when the other app finishes.");
                int phase=NativeReceiver.phase(nativeHandle);
                if(phase<0)throw new IllegalStateException(NativeReceiver.status(nativeHandle));
                long now=System.nanoTime();
                if(track.getTimestamp(stamp))timing.observe(stamp.framePosition,stamp.nanoTime,now,written);
                // A fresh prior hardware observation may bridge a failed query;
                // never invent an anchor or skip rendered frames after lock.
                long dac=timing.project(written,now);
                if(dac<0&&hadTiming)throw new IllegalStateException("Audio presentation timing was lost. Reconnect to recalibrate.");
                if(dac>=0){
                    hadTiming=true;
                    if(budget.observe(dac,now,timing.advances(),timing.observationNs()))NativeReceiver.reportReadiness(nativeHandle,budget.horizonNs(),OutputBudget.BLOCK_NS);
                    NativeReceiver.render(nativeHandle,dac,480,block);
                }
                else {Arrays.fill(block,0f);if(timing.expired(now))throw new IllegalStateException("Reliable AudioTrack timestamps unavailable on this output. Try the phone speaker or a wired output.");}
                if(!budget.ready()&&now-calibrationStarted>5_000_000_000L)throw new IllegalStateException("Audio output latency could not stabilize. Try the phone speaker or wired output, then reconnect.");
                int offset=0;
                while(!cancelled&&offset<block.length){int n=track.write(block,offset,block.length-offset,AudioTrack.WRITE_BLOCKING);if(n<0)throw new IllegalStateException("AudioTrack write failed ("+n+")");if(n==0){Thread.sleep(2);continue;}if(n%2!=0)throw new IllegalStateException("AudioTrack returned an incomplete stereo frame");offset+=n;written+=n/2;}
                int underruns=track.getUnderrunCount();
                if(hadTiming&&underruns>lastUnderruns)throw new IllegalStateException("Audio output underrun. Reconnect to re-establish the playback timeline.");
                lastUnderruns=underruns;
                if(now>=nextStatus){status=(!budget.ready()?"Calibrating measured audio output…":phase==4?"Output calibrated. Waiting for the shared playback timeline…":phase==3?"Rendering against measured audio timestamps":"Waiting for host clock lock…")+"\n"+NativeReceiver.status(nativeHandle)+"\nAudio focus: "+focusState.describe()+"\nAudioTrack buffer: "+track.getBufferSizeInFrames()+" frames · Output underruns: "+underruns+"\nDevice timestamps: "+(dac>=0?"advancing":"not ready");nextStatus=now+500_000_000L;}
                // Fresh clock lock should arrive quickly; silence cannot mask a dead calibration forever.
                if((phase==2||phase==3)&&timelineStarted==0)timelineStarted=now;
                if(timelineStarted!=0&&phase!=3&&now-timelineStarted>15_000_000_000L)throw new IllegalStateException("Host clock could not stabilize. Check LAN congestion and reconnect.");
            }
        }catch(InterruptedException e){Thread.currentThread().interrupt();}
        catch(Throwable e){if(!cancelled){String reason=e.getMessage()==null?e.getClass().getSimpleName():e.getMessage();while(reason.startsWith("Stopped: "))reason=reason.substring(9);stopReason="Stopped: "+reason;}}
        finally {
            config.erase();output=null;
            if(track!=null){if(routeListener!=null)track.removeOnRoutingChangedListener(routeListener);try{track.pause();track.flush();}catch(IllegalStateException ignored){}track.release();}
            long handle=nativeHandle;nativeHandle=0;if(handle!=0){try{NativeReceiver.stop(handle);}catch(Throwable ignored){}}
            main.post(()->{worker=null;status=stopReason;running=false;cleanup();stopSelf();});
        }
    }
    private void onFocusChange(int change){
        if(change==AudioManager.AUDIOFOCUS_GAIN)focusState.gain();
        else if(change==AudioManager.AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK)focusState.duck();
        else if(change==AudioManager.AUDIOFOCUS_LOSS_TRANSIENT)focusState.transientLoss(System.nanoTime());
        else if(change==AudioManager.AUDIOFOCUS_LOSS){cancel("Another app took audio focus. Reconnect when it finishes.");return;}
        else return;
        main.removeCallbacks(focusTimeout);
        if(change==AudioManager.AUDIOFOCUS_LOSS_TRANSIENT)main.postDelayed(focusTimeout,focusState.timeoutDelayMs(System.nanoTime()));
        // The main-thread timeout also cancels a writer blocked by platform
        // auto-pause. Do not pause, flush or replay an old AudioTrack queue on gain. Its
        // continuously advancing timestamp is still validated by the writer.
        AudioTrack track=output;if(track!=null){try{track.setVolume(focusState.volume());}catch(IllegalStateException e){cancel("Audio output changed during the interruption. Reconnect to recalibrate.");}}
    }
    private void cancel(String reason){cancelled=true;stopReason=reason;status="Stopping…";long h=nativeHandle;if(h!=0)NativeReceiver.cancel(h);AudioTrack t=output;if(t!=null){try{t.pause();}catch(IllegalStateException ignored){}}if(worker!=null)worker.interrupt();else{PairingConfig c=takeConfig();if(c!=null)c.erase();running=false;status=reason;cleanup();stopSelf();}}
    private void cleanup(){main.removeCallbacks(focusTimeout);if(focus!=null){manager.abandonAudioFocusRequest(focus);focus=null;}if(noisyRegistered){unregisterReceiver(noisy);noisyRegistered=false;}if(wakeLock!=null){if(wakeLock.isHeld())wakeLock.release();wakeLock=null;}stopForeground(STOP_FOREGROUND_REMOVE);}
    @Override public void onTaskRemoved(Intent root){pairing.clear();cancel("Stopped when the app was closed");super.onTaskRemoved(root);}
    @Override public void onDestroy(){if(worker!=null)cancel(stopReason);else cleanup();super.onDestroy();}
    @Override public IBinder onBind(Intent intent){return null;}
}
