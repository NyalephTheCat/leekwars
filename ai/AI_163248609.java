import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_163248609 extends AI {
public AI_163248609() throws LeekRunException {
super(2, 4);
}
public void staticInit() throws LeekRunException {
}
private double g_EPSILON = 0.0;
private boolean g_init_EPSILON = false;
public Object runIA(Session session) throws LeekRunException {
if (!g_init_EPSILON) { g_EPSILON = (double) 1.0E-9; g_init_EPSILON = true; ops(1); }
return 0l;
}
protected String getAIString() { return "Main_1a99ce438f861_1";}
protected String[] getErrorFiles() { return new String[] {"Main_1a99ce438f861_1", };}

protected int[] getErrorFilesID() { return new int[] {163248609, };}

}
