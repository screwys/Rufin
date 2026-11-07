package io.github.screwys.rufin;

import io.github.screwys.rufin.IDiscoverySession;

interface IDiscoveryService {
    IDiscoverySession open(long timeoutSeconds);
}
