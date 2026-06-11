import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_135843580 extends AI {
public AI_135843580() throws LeekRunException {
super(2, 4);
}
public void staticInit() throws LeekRunException {
}
private double g_EPSILON = 0.0;
private boolean g_init_EPSILON = false;
public Object runIA(Session session) throws LeekRunException {
if (!g_init_EPSILON) { g_EPSILON = (double) 1.0E-9; g_init_EPSILON = true; ops(1); }
ops(2); return (0.5 < g_EPSILON) ? 1l : 0l;
}
protected String getAIString() { return "Main_1a99ceee043c0_6";}
protected String[] getErrorFiles() { return new String[] {"util", "Main_1a99ceee043c0_6", };}

protected int[] getErrorFilesID() { return new int[] {3601347, 135843580, };}

}
