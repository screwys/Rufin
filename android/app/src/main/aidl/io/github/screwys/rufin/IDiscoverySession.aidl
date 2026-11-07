package io.github.screwys.rufin;

import android.os.ParcelFileDescriptor;

oneway interface IDiscoverySession {
    void discover(String request, in ParcelFileDescriptor output);
    void close();
}
