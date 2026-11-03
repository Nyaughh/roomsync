package dev.roomsync;
import java.io.*;
import java.net.DatagramSocket;
import java.util.Arrays;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.locks.LockSupport;

/** Actual JNI/transport/renderer driven by deterministic mock AudioTrack frames.
 * Regression before readiness: 150ms DAC horizon +100ms host lead gave zero of
 * 240,000 nonzero samples and 155,383 concealed frames;250ms gave all240,000.
 * This now verifies the real production host budget makes both preferences safe.
 */
public final class LiveTimingTest {
    static void check(boolean value,String why){if(!value)throw new AssertionError(why);}
    static void exercise(String fixture,int preferredMs,int horizonMs,boolean supported,boolean legacySource)throws Exception{
        Process host=new ProcessBuilder(fixture,Integer.toString(preferredMs),legacySource?"legacy-source":"negotiated-source").redirectError(ProcessBuilder.Redirect.INHERIT).start();
        long handle=0;char[] key=new char[64];Arrays.fill(key,'1');
        try{
            BufferedReader text=new BufferedReader(new InputStreamReader(host.getInputStream()));int port=Integer.parseInt(text.readLine());
            int local;try(DatagramSocket s=new DatagramSocket(0)){local=s.getLocalPort();}
            handle=NativeReceiver.start("127.0.0.1",port,local,key,0);
            long until=System.nanoTime()+3_000_000_000L;
            while(NativeReceiver.phase(handle)==1&&System.nanoTime()<until)Thread.sleep(10);
            check(NativeReceiver.phase(handle)==4,NativeReceiver.status(handle));
            Timing timing=new Timing();OutputBudget budget=new OutputBudget();float[] output=new float[960];long anchor=System.nanoTime(),queueFrames=horizonMs*48L;
            int audible=0,observed=0,projectionMisses=0;boolean failed=false;long[] callbackLateness=new long[440];int callbacks=0;
            for(int block=0;block<440;block++){
                long stamp=anchor+block*10_000_000L;while(System.nanoTime()<stamp)LockSupport.parkNanos(Math.min(500_000L,stamp-System.nanoTime()));
                if(NativeReceiver.phase(handle)<0){failed=true;break;}
                long now=System.nanoTime(),position=block*480L,written=position+queueFrames;
                callbackLateness[callbacks++]=Math.max(0,now-stamp);
                timing.observe(position,stamp,now,written);long dac=timing.project(written,now);
                if(dac<0){if(block>0)projectionMisses++;continue;}
                if(budget.observe(dac,now,timing.advances(),timing.observationNs()))NativeReceiver.reportReadiness(handle,budget.horizonNs(),OutputBudget.BLOCK_NS);
                NativeReceiver.render(handle,dac,480,output);
                if(!budget.ready())check(NativeReceiver.phase(handle)==4,"Timeline must wait for measured output readiness");
                if(block>=300){for(float sample:output){observed++;if(Math.abs(sample)>0.05f)audible++;}}
            }
            String status=NativeReceiver.status(handle);
            Arrays.sort(callbackLateness,0,callbacks);
            System.out.println("Java callback lateness p95="+(callbackLateness[Math.max(0,(callbacks*95+99)/100-1)]/1e6)+"ms max="+(callbackLateness[Math.max(0,callbacks-1)]/1e6)+"ms; projection misses="+projectionMisses);
            NativeReceiver.cancel(handle);NativeReceiver.stop(handle);handle=0;
            if(!host.waitFor(2,TimeUnit.SECONDS))throw new AssertionError("Fixture did not close");
            String line;while((line=text.readLine())!=null)System.out.println(line);
            System.out.println("LiveTimingTest preference="+preferredMs+"ms, measured horizon="+horizonMs+"ms, legacy-source="+legacySource+", audible="+audible+"/"+observed+"\n"+status);
            if(supported){
                check(!failed,status);check(projectionMisses==0,"Mock AudioTrack writer missed an entire hardware deadline; wall-clock fixture overloaded");if(legacySource)check(observed>0&&audible==0,"Old100ms live source availability must reproduce all-concealed silence");
                else check(observed>0&&audible>observed*0.95,"A sufficient shared lead must deliver real PCM");
                check(status.contains("Jitter accepted:")&&status.contains("PCM peak received:"),"Real buffer and PCM diagnostics must be exposed");
                check(status.contains("Shared lead: "+(preferredMs==100?"200.0":"250.0")+" ms"),"Must use production host lead selection");
            }else check(failed&&observed==0,"Unsupported output must fail before audio starts");
        }finally{if(handle!=0){NativeReceiver.cancel(handle);NativeReceiver.stop(handle);}Arrays.fill(key,'\0');if(!host.waitFor(2,TimeUnit.SECONDS)){host.destroyForcibly();host.waitFor();}}
    }
    public static void main(String[] args)throws Exception{exercise(args[0],100,150,true,true);exercise(args[0],100,150,true,false);exercise(args[0],250,150,true,false);exercise(args[0],100,300,false,false);}
}
