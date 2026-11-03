package dev.roomsync;

import android.Manifest;
import android.app.Activity;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.ImageFormat;
import android.os.Bundle;
import android.view.*;
import android.widget.*;
import com.google.zxing.*;
import com.google.zxing.common.HybridBinarizer;
import java.util.*;
import java.util.concurrent.*;
import java.util.concurrent.atomic.AtomicBoolean;

/** Explicit, offline QR scan. Frames are bounded, transient and never persisted. */
@SuppressWarnings("deprecation")
public final class ScanActivity extends Activity implements SurfaceHolder.Callback {
    private SurfaceView surface;
    private TextView message;
    private android.hardware.Camera camera;
    private volatile boolean resumed,completed;
    private boolean surfaceReady;
    private final AtomicBoolean decoding=new AtomicBoolean();
    private final ExecutorService decoder=Executors.newSingleThreadExecutor();
    private long lastFrame;
    @Override public void onCreate(Bundle state){
        super.onCreate(state);getWindow().addFlags(WindowManager.LayoutParams.FLAG_SECURE);
        LinearLayout body=new LinearLayout(this);body.setOrientation(LinearLayout.VERTICAL);body.setPadding(16,24,16,16);
        message=new TextView(this);message.setText("Point the camera at RoomSync’s QR code on Windows. Decoding stays on this phone.");message.setTextSize(18);body.addView(message);
        surface=new SurfaceView(this);body.addView(surface,new LinearLayout.LayoutParams(-1,0,1));surface.getHolder().addCallback(this);
        Button back=new Button(this);back.setText("Cancel scan");back.setOnClickListener(v->finish());body.addView(back);setContentView(body);
        if(android.os.Build.VERSION.SDK_INT>=35)body.setOnApplyWindowInsetsListener((v,insets)->{android.graphics.Insets i=insets.getInsets(WindowInsets.Type.systemBars());v.setPadding(16+i.left,24+i.top,16+i.right,16+i.bottom);return insets;});
        if(checkSelfPermission(Manifest.permission.CAMERA)!=PackageManager.PERMISSION_GRANTED)requestPermissions(new String[]{Manifest.permission.CAMERA},7);
    }
    @Override protected void onResume(){super.onResume();resumed=true;openCamera();}
    @Override protected void onPause(){resumed=false;closeCamera();super.onPause();}
    @Override protected void onDestroy(){closeCamera();decoder.shutdownNow();super.onDestroy();}
    @Override public void onRequestPermissionsResult(int request,String[] permissions,int[] results){super.onRequestPermissionsResult(request,permissions,results);if(request==7){if(results.length>0&&results[0]==PackageManager.PERMISSION_GRANTED)openCamera();else message.setText("Camera access was not granted. Tap Cancel and use Paste setup code instead.");}}
    public void surfaceCreated(SurfaceHolder holder){surfaceReady=true;openCamera();}
    public void surfaceChanged(SurfaceHolder holder,int format,int width,int height){}
    public void surfaceDestroyed(SurfaceHolder holder){surfaceReady=false;closeCamera();}
    private void openCamera(){
        if(!resumed||completed||!surfaceReady||camera!=null||checkSelfPermission(Manifest.permission.CAMERA)!=PackageManager.PERMISSION_GRANTED)return;
        android.hardware.Camera opened=null;
        try{
            int selected=-1;android.hardware.Camera.CameraInfo info=new android.hardware.Camera.CameraInfo();for(int i=0;i<android.hardware.Camera.getNumberOfCameras();i++){android.hardware.Camera.getCameraInfo(i,info);if(info.facing==android.hardware.Camera.CameraInfo.CAMERA_FACING_BACK){selected=i;break;}}
            if(selected<0)throw new IllegalStateException("No rear camera found");
            opened=android.hardware.Camera.open(selected);android.hardware.Camera.Parameters p=opened.getParameters();
            android.hardware.Camera.Size chosen=null;for(android.hardware.Camera.Size size:p.getSupportedPreviewSizes()){if(size.width<=1280&&size.height<=720&&(chosen==null||Math.abs(size.width*size.height-640*480)<Math.abs(chosen.width*chosen.height-640*480)))chosen=size;}
            if(chosen==null)throw new IllegalStateException("No bounded preview format available");
            p.setPreviewSize(chosen.width,chosen.height);p.setPreviewFormat(ImageFormat.NV21);
            List<String> modes=p.getSupportedFocusModes();if(modes!=null&&modes.contains(android.hardware.Camera.Parameters.FOCUS_MODE_CONTINUOUS_PICTURE))p.setFocusMode(android.hardware.Camera.Parameters.FOCUS_MODE_CONTINUOUS_PICTURE);
            opened.setParameters(p);android.hardware.Camera.getCameraInfo(selected,info);int rotation=getWindowManager().getDefaultDisplay().getRotation();int degrees=rotation==Surface.ROTATION_90?90:rotation==Surface.ROTATION_180?180:rotation==Surface.ROTATION_270?270:0;opened.setDisplayOrientation((info.orientation-degrees+360)%360);
            opened.setErrorCallback((error,failed)->{if(failed==camera){closeCamera();message.setText("Camera stopped. Tap Cancel and use Paste setup code instead.");}});
            opened.setPreviewDisplay(surface.getHolder());camera=opened;opened.setPreviewCallback(this::frame);opened.startPreview();
        }catch(Exception e){if(opened!=null){try{opened.release();}catch(RuntimeException ignored){}}camera=null;message.setText("Camera is unavailable. Tap Cancel and paste the Windows setup code instead.");}
    }
    private void frame(byte[] bytes,android.hardware.Camera source){
        if(!resumed||completed||source!=camera||bytes==null)return;
        long now=System.nanoTime();if(now-lastFrame<200_000_000L||!decoding.compareAndSet(false,true))return;lastFrame=now;
        android.hardware.Camera.Size size;try{size=source.getParameters().getPreviewSize();}catch(RuntimeException e){decoding.set(false);return;}
        final int w=size.width,h=size.height;if(w<=0||h<=0||w>1280||h>720||bytes.length<(w*h)||bytes.length>2_000_000){decoding.set(false);return;}
        byte[] copy=bytes.clone();
        try{decoder.execute(()->{try{
            PlanarYUVLuminanceSource luminance=new PlanarYUVLuminanceSource(copy,w,h,0,0,w,h,false);
            Map<DecodeHintType,Object> hints=new EnumMap<>(DecodeHintType.class);hints.put(DecodeHintType.POSSIBLE_FORMATS,Collections.singletonList(BarcodeFormat.QR_CODE));hints.put(DecodeHintType.TRY_HARDER,Boolean.TRUE);
            String value=new MultiFormatReader().decode(new BinaryBitmap(new HybridBinarizer(luminance)),hints).getText();
            PairingConfig parsed=PairingConfig.parse(value,0);parsed.erase();
            runOnUiThread(()->{if(!resumed||completed)return;completed=true;closeCamera();setResult(RESULT_OK,new Intent().putExtra("setup",value));finish();});
        }catch(NotFoundException ignored){}catch(IllegalArgumentException e){runOnUiThread(()->{if(resumed&&!completed)message.setText("That is not a valid RoomSync setup QR. Use the code shown by the updated Windows app.");});}
        catch(RuntimeException ignored){}finally{Arrays.fill(copy,(byte)0);decoding.set(false);}});}catch(RejectedExecutionException e){Arrays.fill(copy,(byte)0);decoding.set(false);}
    }
    private void closeCamera(){android.hardware.Camera c=camera;camera=null;if(c!=null){try{c.setPreviewCallback(null);c.stopPreview();}catch(RuntimeException ignored){}c.release();}}
}
