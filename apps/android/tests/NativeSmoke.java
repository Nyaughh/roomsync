package dev.roomsync;
import java.net.DatagramSocket;
import java.util.Arrays;
public final class NativeSmoke {
    private static void check(boolean b){if(!b)throw new AssertionError();}
    public static void main(String[] args)throws Exception {
        check(NativeReceiver.phase(-1)==0);check("Stopped".equals(NativeReceiver.status(-1)));
        boolean rejected=false;try{NativeReceiver.start("not-an-ip",45450,45451,new char[64],0);}catch(IllegalStateException e){rejected=true;}check(rejected);
        char[] key=new char[64];Arrays.fill(key,'a');
        int localPort;try(DatagramSocket reserve=new DatagramSocket(0)){localPort=reserve.getLocalPort();}
        long handle=NativeReceiver.start("127.0.0.1",45450,localPort,key,0);
        try {
            check(NativeReceiver.phase(handle)==1);
            float[] output=new float[240];Arrays.fill(output,1f);
            NativeReceiver.render(handle,System.nanoTime(),120,output);for(float f:output)check(f==0f);
            rejected=false;try{NativeReceiver.render(handle,System.nanoTime(),961,new float[1922]);}catch(IllegalStateException e){rejected=true;}check(rejected);
        }finally{NativeReceiver.cancel(handle);NativeReceiver.stop(handle);Arrays.fill(key,'\0');}
        check(NativeReceiver.phase(handle)==0);System.out.println("NativeSmoke: JNI loading, validation, real start/cancel/stop and bounded silence passed");
    }
}
