package dev.roomsync;

import android.Manifest;
import android.app.Activity;
import android.content.pm.PackageManager;
import android.graphics.Color;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.text.InputType;
import android.view.View;
import android.view.WindowManager;
import android.widget.*;
import java.util.Arrays;

public final class MainActivity extends Activity {
    private final Handler handler = new Handler(Looper.getMainLooper());
    private TextView status;
    private EditText host, hostPort, localPort, key;
    private Spinner channel;
    private Button connect, stop;
    private final Runnable refresh = new Runnable() {
        public void run() {
            status.setText(ReceiverService.status);
            connect.setEnabled(!ReceiverService.running);
            stop.setEnabled(ReceiverService.running);
            handler.postDelayed(this, 500);
        }
    };
    @Override public void onCreate(Bundle state) {
        super.onCreate(state);
        // Pairing keys must not appear in screenshots, Recents, backups or autofill.
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_SECURE);
        ScrollView scroll = new ScrollView(this);
        LinearLayout body = new LinearLayout(this); body.setOrientation(LinearLayout.VERTICAL);
        body.setPadding(dp(24), dp(28), dp(24), dp(32)); body.setBackgroundColor(Color.rgb(247,246,242));
        scroll.addView(body);
        text(body,"ROOMSYNC",14,Color.rgb(21,125,111));
        text(body,"One room. One beat.",30,Color.rgb(27,38,35));
        text(body,"Android audio receiver · preview\nConnect to your Windows host on the same trusted LAN.",16,Color.DKGRAY);
        host=field(body,"Windows host IP", "192.168.1.10", InputType.TYPE_CLASS_TEXT);
        hostPort=field(body,"Host UDP port", "5000", InputType.TYPE_CLASS_NUMBER); hostPort.setText("5000");
        localPort=field(body,"This phone’s UDP port", "5001", InputType.TYPE_CLASS_NUMBER); localPort.setText("5001");
        key=field(body,"Pairing key (64 hex characters)", "Paste the key created by the Windows host", InputType.TYPE_CLASS_TEXT|InputType.TYPE_TEXT_VARIATION_PASSWORD);
        key.setSaveEnabled(false); key.setImportantForAutofill(View.IMPORTANT_FOR_AUTOFILL_NO);
        key.setFilters(new android.text.InputFilter[]{new android.text.InputFilter.LengthFilter(64)});
        text(body,"Output channel",14,Color.DKGRAY);
        channel=new Spinner(this); channel.setAdapter(new ArrayAdapter<String>(this,android.R.layout.simple_spinner_dropdown_item,new String[]{"Stereo", "Left to both speakers", "Right to both speakers", "Mono mix"})); body.addView(channel);
        connect=new Button(this); connect.setText("Connect & play"); body.addView(connect);
        stop=new Button(this); stop.setText("Stop"); body.addView(stop);
        status=text(body,ReceiverService.status,15,Color.rgb(21,80,70));
        text(body,"Use the phone IP and its UDP port in the host command. Each receiver needs its own host port. No microphone access, account, cloud relay or analytics.\n\nWired / built-in speakers are best. Bluetooth and device DSP add delay that this preview cannot measure. Clock uncertainty is not an end-to-end sync measurement.",13,Color.DKGRAY);
        connect.setOnClickListener(v->connect()); stop.setOnClickListener(v->ReceiverService.requestStop(this,"Stopped by you"));
        setContentView(scroll);
        if (Build.VERSION.SDK_INT >= 35) body.setOnApplyWindowInsetsListener((v,insets)->{android.graphics.Insets i=insets.getInsets(android.view.WindowInsets.Type.systemBars());v.setPadding(dp(24)+i.left,dp(28)+i.top,dp(24)+i.right,dp(32)+i.bottom);return insets;});
    }
    private void connect() {
        char[] secret=new char[key.length()]; key.getText().getChars(0,key.length(),secret,0);
        try {
            String ip=host.getText().toString().trim();
            if(ip.isEmpty()) throw new IllegalArgumentException("Enter the Windows host IP address");
            int hp=Integer.parseInt(hostPort.getText().toString()), lp=Integer.parseInt(localPort.getText().toString());
            if(hp<1||hp>65535||lp<1||lp>65535) throw new IllegalArgumentException("Ports must be 1–65535");
            if(secret.length!=64) throw new IllegalArgumentException("The pairing key must contain 64 hex characters");
            for(char c:secret) if(Character.digit(c,16)<0||c>127) throw new IllegalArgumentException("The pairing key must contain only hexadecimal characters");
            ReceiverService.begin(this,new ReceiverService.Config(ip,hp,lp,secret.clone(),channel.getSelectedItemPosition()));
            key.getText().clear();
            if(Build.VERSION.SDK_INT>=33&&checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)!=PackageManager.PERMISSION_GRANTED) requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS},1);
        } catch(Exception e) { status.setText(e.getMessage()); Toast.makeText(this,e.getMessage(),Toast.LENGTH_LONG).show(); }
        finally { Arrays.fill(secret,'\0'); }
    }
    private EditText field(LinearLayout b,String label,String hint,int type){text(b,label,14,Color.DKGRAY);EditText e=new EditText(this);e.setSingleLine(true);e.setInputType(type);e.setHint(hint);e.setSaveEnabled(false);b.addView(e);return e;}
    private TextView text(LinearLayout b,String value,int size,int color){TextView t=new TextView(this);t.setText(value);t.setTextSize(size);t.setTextColor(color);t.setPadding(0,dp(8),0,dp(8));b.addView(t);return t;}
    private int dp(int n){return Math.round(n*getResources().getDisplayMetrics().density);}
    @Override protected void onResume(){super.onResume();handler.post(refresh);}
    @Override protected void onPause(){handler.removeCallbacks(refresh);super.onPause();}
}
