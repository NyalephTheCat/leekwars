import leekscript.runner.*;
import leekscript.runner.values.*;
import leekscript.runner.classes.*;
import leekscript.common.*;

public class AI_4140 extends AI {
public AI_4140() throws LeekRunException {
super(1, 2);
}
public void staticInit() throws LeekRunException {
}
public Object runIA(Session session) throws LeekRunException {
ops(1); return xor(true, true);
}
protected String getAIString() { return "<snippet 4140>";}
protected String[] getErrorFiles() { return new String[] {"<snippet 4140>", };}

protected int[] getErrorFilesID() { return new int[] {4140, };}

}
